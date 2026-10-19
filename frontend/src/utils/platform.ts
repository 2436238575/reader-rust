// 平台探测（从 ReaderView 抽出，供任何需要 iOS WebKit 特判的地方复用）。

/** iOS WebKit（含 iPad 伪装成 Macintosh 的形态）。 */
export function isIosWebkit(): boolean {
  const ua = typeof navigator !== 'undefined' ? navigator.userAgent : ''
  return (
    /iPhone|iPad|iPod/i.test(ua) ||
    (/Macintosh/i.test(ua) && typeof navigator !== 'undefined' && navigator.maxTouchPoints > 1)
  )
}
