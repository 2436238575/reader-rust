/**
 * 图片灯箱的缩放数学（纯函数，组件只负责接线）。
 *
 * 约定：scale=1 是「适应视口」（图片 max-width/height:100% 的原态），
 * translate 是叠加在居中布局上的像素位移，坐标原点在容器中心。
 */

export const MIN_LIGHTBOX_SCALE = 1
export const MAX_LIGHTBOX_SCALE = 5
/** 指针位移超过这个像素数才算拖动（否则视为点击，触发关闭） */
export const LIGHTBOX_DRAG_THRESHOLD_PX = 5

export interface Point {
  x: number
  y: number
}

export function clampLightboxScale(scale: number): number {
  return Math.min(MAX_LIGHTBOX_SCALE, Math.max(MIN_LIGHTBOX_SCALE, scale))
}

/**
 * 以光标为锚点缩放：保持光标正下方那个图点不动。
 * 图点 p = (cursor - center - translate) / scale，解 translate' 使
 * cursor - center - translate' = p * nextScale。
 */
export function zoomTranslateAt(
  cursor: Point,
  center: Point,
  translate: Point,
  scale: number,
  nextScale: number
): Point {
  const ratio = nextScale / scale
  return {
    x: cursor.x - center.x - (cursor.x - center.x - translate.x) * ratio,
    y: cursor.y - center.y - (cursor.y - center.y - translate.y) * ratio,
  }
}

/** 双指捏合：距离比乘上起手时的 scale。 */
export function pinchScaleAt(startScale: number, startDist: number, nextDist: number): number {
  return clampLightboxScale(startScale * (nextDist / Math.max(1, startDist)))
}

/** 缩放回到适应视口时位移归零（scale≤1 时没有可拖动的余量）。 */
export function normalizeLightboxZoom(scale: number, translate: Point): { scale: number; translate: Point } {
  if (scale <= MIN_LIGHTBOX_SCALE) {
    return { scale: MIN_LIGHTBOX_SCALE, translate: { x: 0, y: 0 } }
  }
  return { scale, translate }
}
