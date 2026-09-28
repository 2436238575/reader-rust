import type { Book } from '../types'
import { safeLocalSet } from './storage'
import { getUserdata, saveUserdata } from '../api/userdata'

const RECENT_BOOKS_KEY = 'reader-recent-books'
// 后端 userdata 文档名（/reader3/getUserdata・saveUserdata），跨浏览器同步用
const BACKEND_DOC_NAME = 'recent-books'
const MAX_RECENT_BOOKS = 100
// 连续保存（快速开书/切章）合并为一次后端写入
const PUSH_DEBOUNCE_MS = 2000

export interface RecentReadBook extends Book {
  recentReadAt: number
}

export function getRecentReadBookKey(book: Pick<Book, 'bookUrl' | 'origin'>) {
  return `${book.origin || ''}::${book.bookUrl || ''}`
}

function isRssEntry(item: Partial<RecentReadBook>) {
  // 旧版本把 RSS 文章也写进这里，RSS 功能移除后这些记录没有可打开的页面
  return (item as { recentKind?: string }).recentKind === 'rss'
}

function normalizeRecentReadBook(book: Book): RecentReadBook {
  const recentReadAt = book.durChapterTime || Date.now()
  return {
    ...book,
    durChapterTime: recentReadAt,
    recentReadAt,
  }
}

function readLocal(): RecentReadBook[] {
  try {
    const raw = localStorage.getItem(RECENT_BOOKS_KEY)
    if (!raw) return []
    const parsed = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    return parsed.filter(
      (item): item is RecentReadBook => !!item?.bookUrl && !!item?.origin && !isRssEntry(item),
    )
  } catch {
    return []
  }
}

function writeLocal(list: RecentReadBook[]) {
  safeLocalSet(RECENT_BOOKS_KEY, JSON.stringify(list.slice(0, MAX_RECENT_BOOKS)))
}

function sortByRecency(list: RecentReadBook[]) {
  return list.sort(
    (a, b) => (b.recentReadAt || b.durChapterTime || 0) - (a.recentReadAt || a.durChapterTime || 0),
  )
}

/** 并集合并：同一本书取 recentReadAt 较新的一条，按时间降序，截断到上限 */
export function mergeRecentLists(
  local: RecentReadBook[],
  remote: RecentReadBook[],
): RecentReadBook[] {
  const byKey = new Map<string, RecentReadBook>()
  const consider = (item: RecentReadBook) => {
    if (!item?.bookUrl || !item?.origin || isRssEntry(item)) return
    const key = getRecentReadBookKey(item)
    const existing = byKey.get(key)
    if (!existing || (item.recentReadAt || 0) >= (existing.recentReadAt || 0)) {
      byKey.set(key, item)
    }
  }
  for (const item of remote) consider(item)
  for (const item of local) consider(item)
  return sortByRecency([...byKey.values()]).slice(0, MAX_RECENT_BOOKS)
}

/** 同步读取本地镜像（首次渲染用）；真实数据以后端合并结果为准 */
export function loadRecentReadBooks(): RecentReadBook[] {
  const list = readLocal()
  writeLocal(sortByRecency([...list]))
  return list
}

let pushTimer: ReturnType<typeof setTimeout> | null = null

function schedulePush() {
  if (pushTimer) clearTimeout(pushTimer)
  pushTimer = setTimeout(() => {
    pushTimer = null
    void saveUserdata(BACKEND_DOC_NAME, readLocal()).catch(() => undefined)
  }, PUSH_DEBOUNCE_MS)
}

// 同步节流：会话内至多每分钟与后端对齐一次，避免每次刷新书架都拉文档
const SYNC_THROTTLE_MS = 60_000
let lastSyncStartedAt = 0
let syncInFlight: Promise<void> | null = null

/** 与后端文档做并集合并，合并结果不一致时推回后端；失败时静默保持本地 */
export async function syncRecentBooksFromBackend(): Promise<void> {
  if (syncInFlight) return syncInFlight
  if (Date.now() - lastSyncStartedAt < SYNC_THROTTLE_MS) return
  lastSyncStartedAt = Date.now()
  syncInFlight = (async () => {
    try {
      let remote: RecentReadBook[] | null
      try {
        remote = await getUserdata<RecentReadBook[]>(BACKEND_DOC_NAME)
      } catch {
        return // 离线或未登录：继续用本地镜像
      }
      const remoteList = Array.isArray(remote) ? remote : []
      const merged = mergeRecentLists(readLocal(), remoteList)
      if (JSON.stringify(merged) !== JSON.stringify(remoteList)) {
        await saveUserdata(BACKEND_DOC_NAME, merged).catch(() => undefined)
      }
      writeLocal(merged)
    } finally {
      syncInFlight = null
    }
  })()
  return syncInFlight
}

export function saveRecentReadBook(book: Book) {
  if (!book?.bookUrl || !book?.origin) return
  const key = getRecentReadBookKey(book)
  const nextEntry = normalizeRecentReadBook(book)
  const next = readLocal().filter((item) => getRecentReadBookKey(item) !== key)
  next.unshift(nextEntry)
  writeLocal(next)
  schedulePush()
}

/** 删除只在当前设备生效（后端无墓碑），其他浏览器会把较旧的记录再合并回来 */
export function removeRecentReadBook(book: Pick<Book, 'bookUrl' | 'origin'>) {
  if (!book?.bookUrl || !book?.origin) return
  const key = getRecentReadBookKey(book)
  const next = readLocal().filter((item) => getRecentReadBookKey(item) !== key)
  writeLocal(next)
  schedulePush()
}

export function clearRecentReadBooks() {
  localStorage.removeItem(RECENT_BOOKS_KEY)
  schedulePush()
}
