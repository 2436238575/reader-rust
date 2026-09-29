import { getUserdata, saveUserdata } from '../api/userdata'

// 后端 userdata 文档名（/reader3/getUserdata・saveUserdata），跨浏览器同步用
export const READING_STATS_DOC_NAME = 'reading-stats'
// readChapters 的保留条数上限（只作累计计数用，超出裁掉最旧的）
export const MAX_READ_CHAPTER_ENTRIES = 5000
// 连续保存合并为一次后端写入
export const STATS_PUSH_DEBOUNCE_MS = 2000

export interface ReadingStats {
  totalSeconds: number
  openedBooks: string[]
  readChapters: string[]
  completedBooks: string[]
}

const LOCAL_STORAGE_KEY = 'reader-stats'

export function emptyReadingStats(): ReadingStats {
  return { totalSeconds: 0, openedBooks: [], readChapters: [], completedBooks: [] }
}

/** 容错解析：任意来源（localStorage / 后端文档）的形状不对时回退默认值 */
export function sanitizeReadingStats(value: unknown): ReadingStats {
  const base = emptyReadingStats()
  if (!value || typeof value !== 'object') return base
  const raw = value as Partial<ReadingStats>
  const dedupe = (list: unknown): string[] =>
    Array.isArray(list) ? [...new Set(list.filter((x): x is string => typeof x === 'string'))] : []
  return {
    totalSeconds:
      typeof raw.totalSeconds === 'number' && raw.totalSeconds > 0
        ? Math.floor(raw.totalSeconds)
        : 0,
    openedBooks: dedupe(raw.openedBooks),
    readChapters: dedupe(raw.readChapters),
    completedBooks: dedupe(raw.completedBooks),
  }
}

/**
 * 双端合并：时长取较大值（各端都持有全量副本，求和会把同一时段翻倍），
 * 集合取并集。离线期间另一端累计的时长会被较大值覆盖，属可接受的近似。
 */
export function mergeReadingStats(local: ReadingStats, remote: ReadingStats): ReadingStats {
  const merged: ReadingStats = {
    totalSeconds: Math.max(local.totalSeconds, remote.totalSeconds),
    openedBooks: [...new Set([...local.openedBooks, ...remote.openedBooks])],
    readChapters: [...new Set([...local.readChapters, ...remote.readChapters])],
    completedBooks: [...new Set([...local.completedBooks, ...remote.completedBooks])],
  }
  // 无上限会随使用年限膨胀且每次同步全量序列化；只统计总数，裁掉最旧的
  if (merged.readChapters.length > MAX_READ_CHAPTER_ENTRIES) {
    merged.readChapters.splice(0, merged.readChapters.length - MAX_READ_CHAPTER_ENTRIES)
  }
  return merged
}

export function loadLocalReadingStats(): ReadingStats {
  try {
    return sanitizeReadingStats(JSON.parse(localStorage.getItem(LOCAL_STORAGE_KEY) || 'null'))
  } catch {
    return emptyReadingStats()
  }
}

export function persistLocalReadingStats(stats: ReadingStats) {
  localStorage.setItem(LOCAL_STORAGE_KEY, JSON.stringify(stats))
}

/** 拉取后端文档并与本地合并；合并结果与后端不一致时推回。失败时静默保持本地 */
export async function syncReadingStatsFromBackend(local: ReadingStats): Promise<ReadingStats> {
  let remote: unknown
  try {
    remote = await getUserdata(READING_STATS_DOC_NAME)
  } catch {
    return local // 离线或未登录
  }
  const merged = mergeReadingStats(local, sanitizeReadingStats(remote))
  if (JSON.stringify(merged) !== JSON.stringify(sanitizeReadingStats(remote))) {
    await saveUserdata(READING_STATS_DOC_NAME, merged).catch(() => undefined)
  }
  persistLocalReadingStats(merged)
  return merged
}

export function pushReadingStatsToBackend(stats: ReadingStats) {
  void saveUserdata(READING_STATS_DOC_NAME, stats).catch(() => undefined)
}
