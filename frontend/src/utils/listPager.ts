/** 页码序列里的占位符：表示两段页码之间有断档 */
export type PagerItem = number | 'ellipsis'

/** 条目数 + 每页大小 → 总页数；空列表与非法 pageSize 都按 1 页兜底 */
export function pageCountOf(total: number, pageSize: number): number {
  const size = Number.isFinite(pageSize) && pageSize > 0 ? Math.floor(pageSize) : 1
  if (!Number.isFinite(total) || total <= 0) return 1
  return Math.ceil(total / size)
}

/** 页码收敛到 [1, totalPages]：列表变短后停在超界页时往回收 */
export function clampPageOf(page: number, totalPages: number): number {
  if (!Number.isFinite(page) || page < 1) return 1
  return Math.min(Math.floor(page), Math.max(1, totalPages))
}

/** 当前页覆盖的条目区间文案（「1 - 50 共 130 个」） */
export function pageRangeText(total: number, pageSize: number, page: number): string {
  if (!Number.isFinite(total) || total <= 0) return '共 0 个'
  const size = Number.isFinite(pageSize) && pageSize > 0 ? Math.floor(pageSize) : 1
  const current = clampPageOf(page, Math.ceil(total / size))
  const start = (current - 1) * size + 1
  const end = Math.min(current * size, total)
  return `${start} - ${end} 共 ${total} 个`
}

/**
 * 页码按钮序列：页数不多时全部平铺；多时保留首末页，
 * 当前页左右各带一个邻页，断档处用 'ellipsis' 占位。
 */
export function buildPageItems(totalPages: number, current: number): PagerItem[] {
  const pages = Math.max(1, totalPages)
  const page = clampPageOf(current, pages)
  if (pages <= 7) {
    return Array.from({ length: pages }, (_, index) => index + 1)
  }
  const items: PagerItem[] = [1]
  const start = Math.max(2, page - 1)
  const end = Math.min(pages - 1, page + 1)
  if (start > 2) items.push('ellipsis')
  for (let p = start; p <= end; p++) items.push(p)
  if (end < pages - 1) items.push('ellipsis')
  items.push(pages)
  return items
}
