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
