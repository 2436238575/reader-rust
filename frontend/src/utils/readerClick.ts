const READER_INTERACTIVE_SELECTOR = [
  'button',
  'a',
  'input',
  'textarea',
  'select',
  '[role="button"]',
  '[contenteditable="true"]',
  '.tts-controls',
  '.mobile-controls',
  '.reader-search-panel',
  '.selection-menu',
].join(', ')

export function isReaderInteractiveClickTarget(
  target: Pick<HTMLElement, 'closest'> | null | undefined
) {
  return Boolean(target?.closest(READER_INTERACTIVE_SELECTOR))
}

/* ─── 阅读器点按热区：窗口 3×3 等分九宫格 ─── */

export type ReaderTapZone = 'prev' | 'menu' | 'next'

// 窗口四边防误触留白（占比），落在留白带里的点按不触发任何动作
const TAP_EDGE_GUARD = 0.04

// 左右翻页：左下区域集中给「上一页」，右侧给「下一页」
const HORIZONTAL_TAP_GRID: ReaderTapZone[][] = [
  ['prev', 'prev', 'next'],
  ['prev', 'menu', 'next'],
  ['prev', 'next', 'next'],
]

// 其他模式（上下滑动/滚动）：上两行偏「上一页」，右下角给「下一页」
const VERTICAL_TAP_GRID: ReaderTapZone[][] = [
  ['prev', 'prev', 'prev'],
  ['prev', 'menu', 'next'],
  ['next', 'next', 'next'],
]

/**
 * 把一次点按映射到热区。返回 null 表示落在四边留白带，不应触发动作。
 * 滑动/滚动翻页不走这里。
 */
export function resolveReaderTapZone(
  clientX: number,
  clientY: number,
  horizontalPaging: boolean,
  width: number,
  height: number
): ReaderTapZone | null {
  if (width <= 0 || height <= 0) return null
  const x = clientX / width
  const y = clientY / height
  if (
    x < TAP_EDGE_GUARD ||
    x > 1 - TAP_EDGE_GUARD ||
    y < TAP_EDGE_GUARD ||
    y > 1 - TAP_EDGE_GUARD
  ) {
    return null
  }
  const col = x < 1 / 3 ? 0 : x < 2 / 3 ? 1 : 2
  const row = y < 1 / 3 ? 0 : y < 2 / 3 ? 1 : 2
  return (horizontalPaging ? HORIZONTAL_TAP_GRID : VERTICAL_TAP_GRID)[row][col]
}
