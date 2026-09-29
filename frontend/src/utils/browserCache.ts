const DB_NAME = 'reader-browser-cache'
const DB_VERSION = 2
const STORE_NAME = 'chapters'
const SUMMARY_STORE = 'summary'
let dbPromise: Promise<IDBDatabase> | null = null

export interface BrowserChapterCacheRecord {
  key: string
  bookUrl: string
  chapterUrl: string
  chapterTitle: string
  content: string
  size: number
  updatedAt: number
}

export interface BrowserBookCacheSummary {
  bookUrl: string
  cachedChapterCount: number
  bytes: number
  updatedAt: number
}

function cacheKey(bookUrl: string, chapterUrl: string) {
  return `${bookUrl}::${chapterUrl}`
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
        const tx = request.transaction!
        if (!db.objectStoreNames.contains(STORE_NAME)) {
          const store = db.createObjectStore(STORE_NAME, { keyPath: 'key' })
          store.createIndex('bookUrl', 'bookUrl', { unique: false })
          store.createIndex('updatedAt', 'updatedAt', { unique: false })
        }
        if (!db.objectStoreNames.contains(SUMMARY_STORE)) {
          db.createObjectStore(SUMMARY_STORE, { keyPath: 'bookUrl' })
          // v1 → v2 迁移：一次性回填每本书的汇总（此后增删都在写路径
          // 增量维护，读取侧再也不用 getAll 把整章正文反序列化进内存）
          const chapters = tx.objectStore(STORE_NAME)
          const summary = tx.objectStore(SUMMARY_STORE)
          const summaryMap = new Map<string, BrowserBookCacheSummary>()
          const cursorRequest = chapters.openCursor()
          cursorRequest.onsuccess = () => {
            const cursor = cursorRequest.result
            if (!cursor) {
              summaryMap.forEach((value) => summary.put(value))
              return
            }
            const record = cursor.value as BrowserChapterCacheRecord
            const current = summaryMap.get(record.bookUrl) || {
              bookUrl: record.bookUrl,
              cachedChapterCount: 0,
              bytes: 0,
              updatedAt: 0,
            }
            current.cachedChapterCount += 1
            current.bytes += record.size || 0
            current.updatedAt = Math.max(current.updatedAt, record.updatedAt || 0)
            summaryMap.set(record.bookUrl, current)
            cursor.continue()
          }
        }
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
  bookUrl: string
): BrowserBookCacheSummary | null {
  const base: BrowserBookCacheSummary = current || {
    bookUrl,
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
  bookUrl: string
): Promise<BrowserBookCacheSummary | undefined> {
  return requestToPromise(summary.get(bookUrl)) as Promise<BrowserBookCacheSummary | undefined>
}

async function writeSummary(summary: IDBObjectStore, bookUrl: string, mutate: SummaryMutator) {
  const current = await readSummary(summary, bookUrl)
  const next = applySummaryDelta(current, mutate, bookUrl)
  if (next) {
    await requestToPromise(summary.put(next))
  } else if (current) {
    await requestToPromise(summary.delete(bookUrl))
  }
}

export async function getBrowserCachedChapter(bookUrl: string, chapterUrl: string) {
  return withStore('readonly', async (store) => {
    const result = await requestToPromise(store.get(cacheKey(bookUrl, chapterUrl)))
    const record = result as BrowserChapterCacheRecord | undefined
    return record?.content || null
  })
}

export async function setBrowserCachedChapter(params: {
  bookUrl: string
  chapterUrl: string
  chapterTitle?: string
  content: string
}) {
  return withStores('readwrite', async ({ chapters, summary }) => {
    const key = cacheKey(params.bookUrl, params.chapterUrl)
    const size = new Blob([params.content]).size
    const updatedAt = Date.now()
    // 先查旧记录拿 size：覆盖写入时汇总只记增量
    const existing = (await requestToPromise(chapters.get(key))) as
      BrowserChapterCacheRecord | undefined
    const record: BrowserChapterCacheRecord = {
      key,
      bookUrl: params.bookUrl,
      chapterUrl: params.chapterUrl,
      chapterTitle: params.chapterTitle || '',
      content: params.content,
      size,
      updatedAt,
    }
    await requestToPromise(chapters.put(record))
    await writeSummary(summary, params.bookUrl, (current) => ({
      ...current,
      cachedChapterCount: current.cachedChapterCount + (existing ? 0 : 1),
      bytes: current.bytes - (existing?.size || 0) + size,
      updatedAt: Math.max(current.updatedAt, updatedAt),
    }))
  })
}

export async function deleteBrowserBookCache(bookUrl: string) {
  return withStores('readwrite', async ({ chapters, summary }) => {
    const index = chapters.index('bookUrl')
    const records = await requestToPromise(index.getAll(IDBKeyRange.only(bookUrl)))
    const removed = records as BrowserChapterCacheRecord[]
    await Promise.all(removed.map((record) => requestToPromise(chapters.delete(record.key))))
    await writeSummary(summary, bookUrl, (current) => ({
      ...current,
      cachedChapterCount: current.cachedChapterCount - removed.length,
      bytes: current.bytes - removed.reduce((total, record) => total + (record.size || 0), 0),
    }))
  })
}

export async function countBrowserBookCache(bookUrl: string) {
  return withStores('readonly', async ({ summary }) => {
    const record = await readSummary(summary, bookUrl)
    return record?.cachedChapterCount || 0
  })
}

export async function listBrowserCachedChapterUrls(bookUrl: string) {
  return withStore('readonly', async (store) => {
    // getAllKeys 只取主键（不含整章正文），主键即 `${bookUrl}::${chapterUrl}`
    const index = store.index('bookUrl')
    const keys = await requestToPromise(index.getAllKeys(IDBKeyRange.only(bookUrl)))
    const prefix = `${bookUrl}::`
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
  return withStores('readwrite', async ({ chapters, summary }) => {
    await requestToPromise(chapters.clear())
    await requestToPromise(summary.clear())
  })
}
