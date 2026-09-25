import type { Book } from '../types'

const RECENT_BOOKS_KEY = 'reader-recent-books'
const MAX_RECENT_BOOKS = 100

export interface RecentReadBook extends Book {
  recentReadAt: number
}

export function getRecentReadBookKey(book: Pick<Book, 'bookUrl' | 'origin'>) {
  return `${book.origin || ''}::${book.bookUrl || ''}`
}

function normalizeRecentReadBook(book: Book): RecentReadBook {
  const recentReadAt = book.durChapterTime || Date.now()
  return {
    ...book,
    durChapterTime: recentReadAt,
    recentReadAt,
  }
}

export function loadRecentReadBooks() {
  try {
    const raw = localStorage.getItem(RECENT_BOOKS_KEY)
    if (!raw) return [] as RecentReadBook[]
    const parsed = JSON.parse(raw)
    if (!Array.isArray(parsed)) return [] as RecentReadBook[]
    const next = parsed
      .filter((item): item is RecentReadBook => !!item?.bookUrl && !!item?.origin)
      // 旧版本把 RSS 文章也写进这里，RSS 功能移除后这些记录没有可打开的页面
      .filter((item) => (item as { recentKind?: string }).recentKind !== 'rss')
      .sort((a, b) => (b.recentReadAt || b.durChapterTime || 0) - (a.recentReadAt || a.durChapterTime || 0))
    localStorage.setItem(RECENT_BOOKS_KEY, JSON.stringify(next.slice(0, MAX_RECENT_BOOKS)))
    return next
  } catch {
    return [] as RecentReadBook[]
  }
}

export function saveRecentReadBook(book: Book) {
  if (!book?.bookUrl || !book?.origin) return
  const key = getRecentReadBookKey(book)
  const nextEntry = normalizeRecentReadBook(book)
  const next = loadRecentReadBooks()
    .filter((item) => getRecentReadBookKey(item) !== key)
  next.unshift(nextEntry)
  localStorage.setItem(RECENT_BOOKS_KEY, JSON.stringify(next.slice(0, MAX_RECENT_BOOKS)))
}

export function removeRecentReadBook(book: Pick<Book, 'bookUrl' | 'origin'>) {
  if (!book?.bookUrl || !book?.origin) return
  const key = getRecentReadBookKey(book)
  const next = loadRecentReadBooks().filter((item) => getRecentReadBookKey(item) !== key)
  localStorage.setItem(RECENT_BOOKS_KEY, JSON.stringify(next))
}

export function clearRecentReadBooks() {
  localStorage.removeItem(RECENT_BOOKS_KEY)
}
