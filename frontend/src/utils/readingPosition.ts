// 阅读位置持久化的纯函数簇：存储键、时间戳归一、服务端存档换算。
// 从 ReaderView 抽出（拆分专项 Phase 0）——无响应式依赖，可单测。

export const READER_POSITION_PREFIX = 'reader-position:'

export interface SavedReadingPosition {
  chapterIndex: number
  progress: number
  paragraphIndex?: number
  paragraphProgress?: number
  updatedAt: number
}

export function getPositionStorageKey(bookId?: string | null) {
  return bookId ? `${READER_POSITION_PREFIX}${bookId}` : ''
}

/// 服务端时间戳有秒/毫秒两种来源，统一归一到毫秒；非法值归 0
export function normalizePositionTimestamp(value?: number | null) {
  if (typeof value !== 'number' || Number.isNaN(value) || value <= 0) return 0
  return value < 1_000_000_000_000 ? value * 1000 : value
}

export interface ServerPositionInput {
  durChapterIndex?: number | null
  durChapterPos?: number | null
  durChapterTime?: number | null
}

/// 从书籍的服务端进度字段换算出存档；书不在当前章时返回 null。
/// durChapterPos 是万分比（0-10000），换算回 0-1。
/// updatedAt 用打开书时捕获的 serverProgressTime：durChapterTime 在打开章节时
/// 已被刷成 Date.now()，直接用它会让服务端在本地/服务端的新旧比较里永远「更新」。
export function buildServerSavedPosition(
  book: ServerPositionInput | null | undefined,
  currentIndex: number,
  serverProgressTime?: number | null
): SavedReadingPosition | null {
  if (!book) return null
  if (book.durChapterIndex !== currentIndex) return null
  const rawPos = typeof book.durChapterPos === 'number' ? book.durChapterPos : 0
  const progress = rawPos > 1 ? rawPos / 10000 : rawPos
  return {
    chapterIndex: currentIndex,
    progress: Math.max(0, Math.min(1, progress || 0)),
    updatedAt: normalizePositionTimestamp(serverProgressTime || book.durChapterTime),
  }
}
