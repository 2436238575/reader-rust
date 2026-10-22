import { getBookmarks, deleteBookmarks, saveBookmarks } from '../api/bookmark'
import {
  deleteBookGroup,
  deleteBooks,
  getBookGroups,
  getBookshelf,
  restoreBooks,
  saveBookGroup,
} from '../api/bookshelf'
import { getReplaceRules, deleteReplaceRules, saveReplaceRules } from '../api/replaceRule'
import { deleteAllBookSources, getBookSources, saveBookSources } from '../api/source'
import type { Book, BookGroup, Bookmark, BookSource, ReplaceRule } from '../types'

const BACKUP_VERSION = 1
const LOCAL_STORAGE_KEYS = [
  'theme',
  'reader-stats',
  'readConfig',
  'reader-themeIndex',
  'reader-isNight',
  'reader-speechConfig',
  'reader-last-session',
  'reader-currentIndex',
  'reader-source-subscriptions',
]

export interface WebdavBackupPayload {
  version: number
  createdAt: string
  app: string
  bookshelf: {
    books: Book[]
    groups: BookGroup[]
  }
  bookSources: BookSource[]
  bookmarks: Bookmark[]
  replaceRules: ReplaceRule[]
  localState: Record<string, string>
}

function captureLocalState() {
  return LOCAL_STORAGE_KEYS.reduce<Record<string, string>>((acc, key) => {
    const value = localStorage.getItem(key)
    if (value != null) {
      acc[key] = value
    }
    return acc
  }, {})
}

function applyLocalState(localState: Record<string, string> = {}) {
  LOCAL_STORAGE_KEYS.forEach((key) => {
    const value = localState[key]
    if (value == null) {
      localStorage.removeItem(key)
    } else {
      localStorage.setItem(key, value)
    }
  })
}

export async function createWebdavBackupPayload(): Promise<WebdavBackupPayload> {
  const [books, groups, bookSources, bookmarks, replaceRules] = await Promise.all([
    getBookshelf(),
    getBookGroups(),
    getBookSources(),
    getBookmarks(),
    getReplaceRules(),
  ])

  return {
    version: BACKUP_VERSION,
    createdAt: new Date().toISOString(),
    app: 'reader-rust-frontend',
    bookshelf: {
      books,
      groups,
    },
    bookSources,
    bookmarks,
    replaceRules,
    localState: captureLocalState(),
  }
}

export function serializeWebdavBackup(payload: WebdavBackupPayload) {
  return JSON.stringify(payload, null, 2)
}

export function parseWebdavBackup(raw: string) {
  const payload = JSON.parse(raw) as Partial<WebdavBackupPayload>
  if (!payload || typeof payload !== 'object') {
    throw new Error('备份文件格式无效')
  }
  if (!payload.version || !payload.bookshelf) {
    throw new Error('备份文件缺少必要字段')
  }
  return {
    version: payload.version,
    createdAt: payload.createdAt || new Date().toISOString(),
    app: payload.app || 'reader-rust-frontend',
    bookshelf: {
      books: payload.bookshelf.books || [],
      groups: payload.bookshelf.groups || [],
    },
    bookSources: payload.bookSources || [],
    bookmarks: payload.bookmarks || [],
    replaceRules: payload.replaceRules || [],
    localState: payload.localState || {},
  } as WebdavBackupPayload
}

/**
 * 位域分组的合法 id：2 的幂且不超过 2^30（JS 位运算是 32 位有符号整数，
 * 1<<31 就是负号）。旧版 max_id+1 分配的备份里可能混着 3 这种非法 id
 *（0b11 = 同时进组 1 和组 2），恢复时直接落库会把老 bug 带回来。
 */
function isValidGroupId(id: number) {
  return Number.isInteger(id) && id > 0 && id <= 2 ** 30 && (id & (id - 1)) === 0
}

export async function restoreWebdavBackup(payload: WebdavBackupPayload) {
  const currentGroups = await getBookGroups().catch(() => [])
  const currentBooks = await getBookshelf().catch(() => [])
  const currentBookmarks = await getBookmarks().catch(() => [])
  const currentReplaceRules = await getReplaceRules().catch(() => [])

  await Promise.all([
    currentGroups.length
      ? Promise.all(currentGroups.map((group) => deleteBookGroup(group.groupId)))
      : Promise.resolve(),
    currentBooks.length
      ? deleteBooks(currentBooks.map((book) => book.bookId!).filter(Boolean))
      : Promise.resolve(),
    currentBookmarks.length ? deleteBookmarks(currentBookmarks) : Promise.resolve(),
    currentReplaceRules.length ? deleteReplaceRules(currentReplaceRules) : Promise.resolve(),
    deleteAllBookSources().catch(() => undefined),
  ])

  if (payload.bookSources.length) {
    await saveBookSources(payload.bookSources)
  }
  // 非法组 id 丢弃，书籍位掩码里对应的位一并剥掉，避免恢复出悬空分组引用
  const validGroups = payload.bookshelf.groups.filter((group) => isValidGroupId(group.groupId))
  for (const group of validGroups) {
    await saveBookGroup(group)
  }
  const validGroupMask = validGroups.reduce((mask, group) => mask | group.groupId, 0)
  if (payload.bookshelf.books.length) {
    // 恢复 = 全量覆盖：restore 语义让后端清表重建登记表，备份自带的
    // bookId 原位恢复，书签等按 id 引用的数据随之对齐
    const books = payload.bookshelf.books.map((book) => ({
      ...book,
      group: (book.group ?? 0) & validGroupMask,
    }))
    await restoreBooks(books)
  }
  if (payload.bookmarks.length) {
    await saveBookmarks(payload.bookmarks)
  }
  if (payload.replaceRules.length) {
    await saveReplaceRules(payload.replaceRules)
  }

  applyLocalState(payload.localState)
}
