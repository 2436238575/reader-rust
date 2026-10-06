/** 字节数格式化（缓存管理 / WebDAV 等共用）。 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unitIndex = 0
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024
    unitIndex += 1
  }
  return `${value >= 10 || unitIndex === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unitIndex]}`
}

/** 时间戳格式化为本地日期时间；空值/非法值统一返回 `-`。 */
export function formatDateTime(ts: number | string | null | undefined): string {
  if (ts === null || ts === undefined || ts === '') return '-'
  const date = new Date(ts)
  return Number.isNaN(date.getTime()) ? '-' : date.toLocaleString()
}

/** 时间戳（毫秒）格式化为 `M-D H:mm` 短形态（书签列表等紧凑场景）。 */
export function formatMonthDayTime(ts: number | null | undefined): string {
  if (!ts) return ''
  const date = new Date(ts)
  if (Number.isNaN(date.getTime())) return ''
  return `${date.getMonth() + 1}-${date.getDate()} ${date.getHours()}:${String(date.getMinutes()).padStart(2, '0')}`
}

/**
 * 目录章节的 updateTime 展示：书源给纯数字时间戳（10 位秒 / 13 位毫秒）时格式化为
 * `YY-MM-DD`；其余文本（站点自己排版的日期）原样返回。空值返回空串。
 */
export function formatChapterTag(tag: string | null | undefined): string {
  if (!tag) return ''
  const trimmed = tag.trim()
  if (!/^\d{10}$|^\d{13}$/.test(trimmed)) return trimmed
  const ms = trimmed.length === 10 ? Number(trimmed) * 1000 : Number(trimmed)
  const date = new Date(ms)
  if (Number.isNaN(date.getTime())) return trimmed
  const yy = String(date.getFullYear()).slice(-2)
  const mm = String(date.getMonth() + 1).padStart(2, '0')
  const dd = String(date.getDate()).padStart(2, '0')
  return `${yy}-${mm}-${dd}`
}

/** 目录章节的字数展示：纯数字补「字」后缀，已带单位的文本原样返回。 */
export function formatChapterWordCount(wordCount: string | null | undefined): string {
  if (!wordCount) return ''
  const trimmed = wordCount.trim()
  if (!trimmed) return ''
  return /^\d+$/.test(trimmed) ? `${trimmed} 字` : trimmed
}
