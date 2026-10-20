// 横翻分页的 DOM 可测纯函数簇（从 useHorizontalPaging 抽出，拆分专项 Phase 0）。
// 测量/翻页状态机仍在 composable；这里的每个函数都不依赖运行时测量，
// jsdom 下可完整单测。

export const HORIZONTAL_PAGE_SIDE_PADDING = 24
export const HORIZONTAL_PAGE_VERTICAL_PADDING = 24
export const HORIZONTAL_PAGE_MIN_BOTTOM_GUARD = 18
export const HORIZONTAL_PAGE_BOTTOM_GUARD_LINES = 0.5
// 桌面端左侧有 64px 固定 sidebar（>767px 时存在）：横翻内容区两侧各留 88px，
// 居中后不被遮挡
export const HORIZONTAL_DESKTOP_SIDE_CLEARANCE = 88
// 双页翻页的最小容器宽度：两列都要保持可读宽度
export const DUAL_PAGE_MIN_WIDTH = 960

const NON_STARTING_PUNCTUATION = new Set([
  '，',
  '。',
  '、',
  '：',
  '；',
  '！',
  '？',
  ',',
  '.',
  ':',
  ';',
  '!',
  '?',
  '”',
  '’',
  '）',
  '】',
  '》',
  '」',
  '』',
])
const SPLIT_BACKTRACK_BREAKERS = new Set([
  '，',
  '。',
  '；',
  '！',
  '？',
  ',',
  '.',
  ';',
  '!',
  '?',
  ' ',
])
const PUNCTUATION_ONLY_CHARS = new Set([
  ...NON_STARTING_PUNCTUATION,
  '“',
  '‘',
  '（',
  '【',
  '《',
  '「',
  '『',
  '…',
  '—',
])
const MAX_PUNCTUATION_BACKTRACK = 12

export function escapePagingHtml(input: string) {
  return input.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
}

export function parseInlineStyle(styleText: string) {
  const entries = styleText
    .split(';')
    .map((item) => item.trim())
    .filter(Boolean)
    .map((item) => {
      const idx = item.indexOf(':')
      if (idx <= 0) return null
      const key = item.slice(0, idx).trim().toLowerCase()
      const value = item.slice(idx + 1).trim()
      if (!key || !value) return null
      return [key, value] as const
    })
    .filter(Boolean) as Array<readonly [string, string]>
  return Object.fromEntries(entries) as Record<string, string>
}

export function buildInlineStyle(styleObj: Record<string, string>) {
  return Object.entries(styleObj)
    .map(([key, value]) => `${key}: ${value}`)
    .join('; ')
}

export function removeIndentClass(className: string) {
  return className
    .split(/\s+/)
    .filter(Boolean)
    .filter((name) => name !== 'reader-indent')
    .join(' ')
}

export function normalizeParagraphHtml(paragraphHtml: string) {
  const wrapper = document.createElement('div')
  wrapper.innerHTML = paragraphHtml
  const paragraph = wrapper.querySelector('p')
  if (!paragraph) return paragraphHtml
  const style = paragraph.getAttribute('style') || ''
  const styleObj = parseInlineStyle(style)
  if (paragraph.classList.contains('reader-indent') && !styleObj['text-indent']) {
    styleObj['text-indent'] = '2em'
  }
  const styleText = buildInlineStyle(styleObj)
  if (styleText) paragraph.setAttribute('style', styleText)
  return paragraph.outerHTML
}

export function parseParagraphHtml(paragraphHtml: string) {
  const wrapper = document.createElement('div')
  wrapper.innerHTML = paragraphHtml
  const paragraph = wrapper.querySelector('p')
  if (!paragraph) return null
  return {
    style: paragraph.getAttribute('style') || '',
    className: paragraph.getAttribute('class') || '',
    text: (paragraph.textContent || '').trimEnd(),
  }
}

/**
 * 段评气泡不参与分页：测量器里气泡的 SVG 没有 scoped 样式时会按 300×150
 * 默认尺寸撑爆段落高度（正文只显示半屏的根因），按字符拆段时气泡文本又会
 * 被拍平成正文里的裸数字。分页前摘下，段落全部放定后挂回末段。
 */
export function stripParaCommentBubble(blockHtml: string) {
  if (!blockHtml.includes('para-comment-bubble')) return { html: blockHtml, bubbleHtml: '' }
  const wrapper = document.createElement('div')
  wrapper.innerHTML = blockHtml
  const paragraph = wrapper.querySelector('p')
  const bubble = wrapper.querySelector('.para-comment-bubble')
  if (!paragraph || !bubble) return { html: blockHtml, bubbleHtml: '' }
  const bubbleHtml = bubble.outerHTML
  bubble.remove()
  return { html: paragraph.outerHTML, bubbleHtml }
}

export function appendBubbleToBlock(blockHtml: string, bubbleHtml: string) {
  const idx = blockHtml.lastIndexOf('</p>')
  if (idx < 0) return `${blockHtml}${bubbleHtml}`
  return `${blockHtml.slice(0, idx)}${bubbleHtml}${blockHtml.slice(idx)}`
}

export function buildParagraphHtml(style: string, text: string, className = '') {
  const stylePart = style ? ` style="${style}"` : ''
  const classPart = className ? ` class="${className}"` : ''
  return `<p${classPart}${stylePart}>${escapePagingHtml(text)}</p>`
}

export function firstVisibleChar(text: string) {
  return text.trimStart().charAt(0)
}

export function isPunctuationOnlyText(text: string) {
  const value = text.trim()
  return (
    value.length > 0 &&
    value.length <= 3 &&
    Array.from(value).every((char) => PUNCTUATION_ONLY_CHARS.has(char))
  )
}

/// 段尾切开时下一页若以标点开头（阅读体验差），沿原文回溯到最近的分断点
export function adjustFitCountForReadableStart(text: string, fitCount: number) {
  if (fitCount <= 0 || fitCount >= text.length) return fitCount
  if (!NON_STARTING_PUNCTUATION.has(firstVisibleChar(text.slice(fitCount)))) return fitCount

  const minIndex = Math.max(1, fitCount - MAX_PUNCTUATION_BACKTRACK)
  for (let idx = fitCount - 1; idx >= minIndex; idx -= 1) {
    if (SPLIT_BACKTRACK_BREAKERS.has(text[idx])) {
      return idx + 1
    }
  }
  return Math.max(1, fitCount - Math.min(4, fitCount - 1))
}

export function extractTextFromHtml(html: string) {
  const wrapper = document.createElement('div')
  wrapper.innerHTML = html
  return wrapper.textContent || ''
}

export function prependTextToFirstParagraph(html: string, text: string) {
  const wrapper = document.createElement('div')
  wrapper.innerHTML = html
  const paragraph = wrapper.querySelector('p')
  if (!paragraph) return `${escapePagingHtml(text)}${html}`
  paragraph.insertBefore(document.createTextNode(text), paragraph.firstChild)
  return wrapper.innerHTML
}

export function appendTextToLastParagraph(html: string, text: string) {
  const wrapper = document.createElement('div')
  wrapper.innerHTML = html
  const paragraphs = wrapper.querySelectorAll('p')
  const paragraph = paragraphs[paragraphs.length - 1]
  if (!paragraph) return `${html}${escapePagingHtml(text)}`
  paragraph.appendChild(document.createTextNode(text))
  return wrapper.innerHTML
}

/// 孤标点页合并：整页只剩标点的页并入前一页（或前移到下一页），
/// 避免出现「一页只有一个句号」
export function mergeOrphanPunctuationPages(sourcePages: string[]) {
  const pendingPages = [...sourcePages]
  const mergedPages: string[] = []
  for (let idx = 0; idx < pendingPages.length; idx += 1) {
    const page = pendingPages[idx]
    const text = extractTextFromHtml(page)
    if (isPunctuationOnlyText(text)) {
      const punctuation = text.trim()
      if (mergedPages.length) {
        mergedPages[mergedPages.length - 1] = appendTextToLastParagraph(
          mergedPages[mergedPages.length - 1],
          punctuation
        )
        continue
      }
      if (idx + 1 < pendingPages.length) {
        pendingPages[idx + 1] = prependTextToFirstParagraph(pendingPages[idx + 1], punctuation)
        continue
      }
    }
    mergedPages.push(page)
  }
  return mergedPages
}
