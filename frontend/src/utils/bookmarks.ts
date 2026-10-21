import type { Bookmark } from '../types'

/**
 * 书签分型与 chapterPos 换算的纯函数。
 *
 * chapterPos 是万分比章内进度（0–10000）：章书签恒 0，划线取收藏那一刻的
 * 章内进度。旧数据没有 kind 字段，一律按章书签归组。
 */

export type BookmarkKind = 'bookmark' | 'underline'

export function bookmarkKindOf(bm: Pick<Bookmark, 'kind'>): BookmarkKind {
  return bm.kind === 'underline' ? 'underline' : 'bookmark'
}

export function splitBookmarksByKind(bookmarks: Bookmark[]): {
  bookmarks: Bookmark[]
  underlines: Bookmark[]
} {
  const result: { bookmarks: Bookmark[]; underlines: Bookmark[] } = {
    bookmarks: [],
    underlines: [],
  }
  for (const bm of bookmarks) {
    if (bookmarkKindOf(bm) === 'underline') {
      result.underlines.push(bm)
    } else {
      result.bookmarks.push(bm)
    }
  }
  return result
}

/** 0–1 进度 → 万分比 chapterPos（夹取 + 取整） */
export function progressToChapterPos(progress: number): number {
  return Math.round(Math.max(0, Math.min(1, progress)) * 10000)
}

/** 万分比 chapterPos → 0–1 进度（缺省/越界按章首处理） */
export function chapterPosToProgress(pos?: number): number {
  return Math.max(0, Math.min(1, (pos || 0) / 10000))
}
