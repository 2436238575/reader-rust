const DB_NAME = 'reader-browser-cache'
// v3 → v4：缓存键从 bookUrl 换为 bookId（稳定身份，不随换源变化）。
// 纯缓存无迁移价值，升级时整库重建
const DB_VERSION = 4
const STORE_NAME = 'chapters'
const SUMMARY_STORE = 'summary'
// 目录缓存：{ bookId, chapters, updatedAt }，离线时阅读器/详情弹窗回退用
const CATALOG_STORE = 'catalogs'
let dbPromise: Promise<IDBDatabase> | null = null

export interface BrowserChapterCacheRecord {
  key: string
  bookId: string
  chapterUrl: string
  chapterTitle: string
  content: string
  size: number
  updatedAt: number
}

export interface BrowserBookCacheSummary {
  bookId: string
  cachedChapterCount: number
  bytes: number
  updatedAt: number
}

function cacheKey(bookId: string, chapterUrl: string) {
  return `${bookId}::${chapterUrl}`
}

function openDb(): Promise<IDBDatabase> {
  if (!dbPromise) {
    dbPromise = new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open(DB_NAME, DB_VERSION)

      request.onerror = () => reject(request.error)
      request.onsuccess = () => {
        const db = request.result
        db.onversionchange = () => {
          db.close()
          dbPromise = null
        }
        resolve(db)
      }
      request.onupgradeneeded = () => {
        const db = request.result
        // 缓存键整体换成 bookId：旧数据不可迁移，直接清掉重建
        for (const name of [STORE_NAME, SUMMARY_STORE, CATALOG_STORE]) {
          if (db.objectStoreNames.contains(name)) db.deleteObjectStore(name)
        }
        const store = db.createObjectStore(STORE_NAME, { keyPath: 'key' })
        store.createIndex('bookId', 'bookId', { unique: false })
        store.createIndex('updatedAt', 'updatedAt', { unique: false })
        db.createObjectStore(SUMMARY_STORE, { keyPath: 'bookId' })
        db.createObjectStore(CATALOG_STORE, { keyPath: 'bookId' })
      }
    }).catch((error: unknown) => {
      dbPromise = null
      throw error
    })
  }
  return dbPromise!
}

async function withStore<T>(
  mode: IDBTransactionMode,
  handler: (store: IDBObjectStore) => Promise<T>
): Promise<T> {
  return withStoreIn(STORE_NAME, mode, handler)
}

async function withStoreIn<T>(
  storeName: string,
  mode: IDBTransactionMode,
  handler: (store: IDBObjectStore) => Promise<T>
): Promise<T> {
  const db = await openDb()
  return new Promise<T>((resolve, reject) => {
    const tx = db.transaction(storeName, mode)
    const store = tx.objectStore(storeName)

    handler(store)
      .then((result) => {
        tx.oncomplete = () => {
          resolve(result)
        }
        tx.onerror = () => {
          reject(tx.error)
        }
      })
      .catch((error) => {
        reject(error)
      })
  })
}

async function withStores<T>(
  mode: IDBTransactionMode,
  handler: (stores: { chapters: IDBObjectStore; summary: IDBObjectStore }) => Promise<T>
): Promise<T> {
  const db = await openDb()
  return new Promise<T>((resolve, reject) => {
    const tx = db.transaction([STORE_NAME, SUMMARY_STORE], mode)
    const stores = {
      chapters: tx.objectStore(STORE_NAME),
      summary: tx.objectStore(SUMMARY_STORE),
    }

    handler(stores)
      .then((result) => {
        tx.oncomplete = () => {
          resolve(result)
        }
        tx.onerror = () => {
          reject(tx.error)
        }
      })
      .catch((error) => {
        reject(error)
      })
  })
}

function requestToPromise<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result)
    request.onerror = () => reject(request.error)
  })
}

type SummaryMutator = (summary: BrowserBookCacheSummary) => BrowserBookCacheSummary

function applySummaryDelta(
  current: BrowserBookCacheSummary | undefined,
  mutate: SummaryMutator,
  bookId: string
): BrowserBookCacheSummary | null {
  const base: BrowserBookCacheSummary = current || {
    bookId,
    cachedChapterCount: 0,
    bytes: 0,
    updatedAt: 0,
  }
  const next = mutate({ ...base })
  if (next.cachedChapterCount <= 0 || next.bytes <= 0) {
    return null
  }
  return next
}

async function readSummary(
  summary: IDBObjectStore,
  bookId: string
): Promise<BrowserBookCacheSummary | undefined> {
  return requestToPromise(summary.get(bookId)) as Promise<BrowserBookCacheSummary | undefined>
}

async function writeSummary(summary: IDBObjectStore, bookId: string, mutate: SummaryMutator) {
  const current = await readSummary(summary, bookId)
  const next = applySummaryDelta(current, mutate, bookId)
  if (next) {
    await requestToPromise(summary.put(next))
  } else if (current) {
    await requestToPromise(summary.delete(bookId))
  }
}

export async function getBrowserCachedChapter(bookId: string, chapterUrl: string) {
  return withStore('readonly', async (store) => {
    const result = await requestToPromise(store.get(cacheKey(bookId, chapterUrl)))
    const record = result as BrowserChapterCacheRecord | undefined
    return record?.content || null
  })
}

export async function setBrowserCachedChapter(params: {
  bookId: string
  chapterUrl: string
  chapterTitle?: string
  content: string
}) {
  try {
    return await writeChapterRecord(params)
  } catch (error) {
    // 配额写满：淘汰最久未更新的一本（非当前书）再试一次；
    // 没有可淘汰的或重试仍失败，错误原样上抛（调用方中断任务）
    if (!isQuotaExceeded(error)) throw error
    const evicted = await evictOldestOtherBook(params.bookId)
    if (!evicted) throw error
    return writeChapterRecord(params)
  }
}

function isQuotaExceeded(error: unknown) {
  return error instanceof DOMException && error.name === 'QuotaExceededError'
}

/// 淘汰最久未更新的一本书的整书缓存；无可淘汰时返回 false
async function evictOldestOtherBook(currentBookId: string): Promise<boolean> {
  // listBrowserCacheSummary 按 updatedAt 倒序，最后一个最旧
  const summaries = await listBrowserCacheSummary()
  const oldest = [...summaries].reverse().find((item) => item.bookId !== currentBookId)
  if (!oldest) return false
  await deleteBrowserBookCache(oldest.bookId)
  return true
}

function writeChapterRecord(params: {
  bookId: string
  chapterUrl: string
  chapterTitle?: string
  content: string
}) {
  return withStores('readwrite', async ({ chapters, summary }) => {
    const key = cacheKey(params.bookId, params.chapterUrl)
    const size = new Blob([params.content]).size
    const updatedAt = Date.now()
    // 先查旧记录拿 size：覆盖写入时汇总只记增量
    const existing = (await requestToPromise(chapters.get(key))) as
      BrowserChapterCacheRecord | undefined
    const record: BrowserChapterCacheRecord = {
      key,
      bookId: params.bookId,
      chapterUrl: params.chapterUrl,
      chapterTitle: params.chapterTitle || '',
      content: params.content,
      size,
      updatedAt,
    }
    await requestToPromise(chapters.put(record))
    await writeSummary(summary, params.bookId, (current) => ({
      ...current,
      cachedChapterCount: current.cachedChapterCount + (existing ? 0 : 1),
      bytes: current.bytes - (existing?.size || 0) + size,
      updatedAt: Math.max(current.updatedAt, updatedAt),
    }))
  })
}

export async function deleteBrowserBookCache(bookId: string) {
  await withStores('readwrite', async ({ chapters, summary }) => {
    const index = chapters.index('bookId')
    const records = await requestToPromise(index.getAll(IDBKeyRange.only(bookId)))
    const removed = records as BrowserChapterCacheRecord[]
    await Promise.all(removed.map((record) => requestToPromise(chapters.delete(record.key))))
    await writeSummary(summary, bookId, (current) => ({
      ...current,
      cachedChapterCount: current.cachedChapterCount - removed.length,
      bytes: current.bytes - removed.reduce((total, record) => total + (record.size || 0), 0),
    }))
  })
  // 目录缓存跟随该书一起清
  await withStoreIn(CATALOG_STORE, 'readwrite', async (store) => {
    await requestToPromise(store.delete(bookId))
  }).catch(() => undefined)
}

export async function countBrowserBookCache(bookId: string) {
  return withStores('readonly', async ({ summary }) => {
    const record = await readSummary(summary, bookId)
    return record?.cachedChapterCount || 0
  })
}

export async function listBrowserCachedChapterUrls(bookId: string) {
  return withStore('readonly', async (store) => {
    // getAllKeys 只取主键（不含整章正文），主键即 `${bookId}::${chapterUrl}`
    const index = store.index('bookId')
    const keys = await requestToPromise(index.getAllKeys(IDBKeyRange.only(bookId)))
    const prefix = `${bookId}::`
    return new Set(
      (keys as IDBValidKey[])
        .map((key) => (typeof key === 'string' ? key.slice(prefix.length) : ''))
        .filter(Boolean)
    )
  })
}

export async function listBrowserCacheSummary(): Promise<BrowserBookCacheSummary[]> {
  return withStoreIn(SUMMARY_STORE, 'readonly', async (store) => {
    const records = await requestToPromise(store.getAll())
    return (records as BrowserBookCacheSummary[]).sort((a, b) => b.updatedAt - a.updatedAt)
  })
}

export async function clearAllBrowserCache() {
  await withStores('readwrite', async ({ chapters, summary }) => {
    await requestToPromise(chapters.clear())
    await requestToPromise(summary.clear())
  })
  // 目录缓存一并清掉
  await withStoreIn(CATALOG_STORE, 'readwrite', async (store) => {
    await requestToPromise(store.clear())
  }).catch(() => undefined)
}

/* ─── 目录缓存：离线时阅读器/详情弹窗回退到最近一次成功拉到的目录 ─── */

export interface BrowserCatalogRecord<ChapterT = unknown> {
  bookId: string
  chapters: ChapterT[]
  updatedAt: number
}

export async function getBrowserCatalog<ChapterT>(bookId: string): Promise<ChapterT[] | null> {
  if (!bookId) return null
  return withStoreIn(CATALOG_STORE, 'readonly', async (store) => {
    const record = (await requestToPromise(store.get(bookId))) as
      BrowserCatalogRecord<ChapterT> | undefined
    return record?.chapters?.length ? record.chapters : null
  }).catch(() => null)
}

export async function setBrowserCatalog<ChapterT>(bookId: string, chapters: ChapterT[]) {
  if (!bookId || !chapters.length) return
  return withStoreIn(CATALOG_STORE, 'readwrite', async (store) => {
    const record: BrowserCatalogRecord<ChapterT> = { bookId, chapters, updatedAt: Date.now() }
    await requestToPromise(store.put(record))
  }).catch(() => undefined)
}
