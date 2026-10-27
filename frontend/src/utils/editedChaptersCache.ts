// 已编辑章节集合的本地快照读写：从 reader store 拆出的纯函数（reader store 拆分专项）。
// 离线打开书时 listChapterEdits 拉不到，快照维持编辑按钮的状态展示（缓存读写
// 不走它：浏览器缓存记录带版本号，会不会串版本由在线读取时的比对兜底）。
// 只保留最近 20 本书；写满或数据损坏时静默放弃，在线打开会重新同步。

import { safeLocalSet } from './storage'

const EDITED_CHAPTERS_CACHE_KEY = 'reader-edited-chapters'
const EDITED_CHAPTERS_CACHE_MAX_BOOKS = 20

export interface EditedChaptersCacheEntry {
  urls: string[]
  savedAt: number
}

export function readEditedChaptersCache(): Record<string, EditedChaptersCacheEntry> {
  try {
    const raw = localStorage.getItem(EDITED_CHAPTERS_CACHE_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw) as Record<string, EditedChaptersCacheEntry>
    return parsed && typeof parsed === 'object' ? parsed : {}
  } catch {
    return {}
  }
}

export function loadCachedEditedChapterKeys(bookId: string): string[] {
  const entry = readEditedChaptersCache()[bookId]
  return Array.isArray(entry?.urls)
    ? entry.urls.filter((url): url is string => typeof url === 'string')
    : []
}

export function saveCachedEditedChapterKeys(bookId: string, urls: string[]) {
  try {
    const cache = readEditedChaptersCache()
    cache[bookId] = { urls, savedAt: Date.now() }
    const trimmed = Object.entries(cache)
      .sort((a, b) => (b[1]?.savedAt || 0) - (a[1]?.savedAt || 0))
      .slice(0, EDITED_CHAPTERS_CACHE_MAX_BOOKS)
    safeLocalSet(EDITED_CHAPTERS_CACHE_KEY, JSON.stringify(Object.fromEntries(trimmed)))
  } catch {
    /* 快照只是离线兜底，写不进去不影响在线阅读 */
  }
}
