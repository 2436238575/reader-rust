// 阅读器正文渲染的纯函数簇（从 ReaderView.vue 抽出）：
// 消毒/段落样式的基础渲染、章节配图插入、段评气泡、EPUB 资源改写、搜索高亮。
// 全部是「输入 → DOM/HTML 字符串」的确定性函数，不持有响应式状态。

import { withAuthQuery } from '../api/bookshelf'
import { sanitizeUntrustedHtml } from './sanitize'
import type { ChapterImage, ParaReviewCount } from '../types'

/** 基础渲染的排版选项（来自阅读器配置）。 */
export interface BaseRenderOptions {
  showChapterImages: boolean
  firstLineIndent: boolean
  paragraphSpacing: number
}

export function escapeHtmlText(value: string) {
  return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

/** 属性值转义：除了 `&<>` 还要挡引号，否则能拼出新的属性。 */
export function escapeHtmlAttr(value: string) {
  return escapeHtmlText(value).replace(/"/g, '&quot;').replace(/'/g, '&#39;')
}

export function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/** 按 `\n` 切分正文，返回非空行的「原始行号 + 文本」。 */
export function splitParagraphLines(text: string) {
  const lines: { index: number; text: string }[] = []
  if (!text) return lines
  text.split(/\n/).forEach((line, index) => {
    const trimmed = line.replace(/^[\u3000\u00A0 \t]+/, '').trim()
    if (trimmed) lines.push({ index, text: trimmed })
  })
  return lines
}

// 基础渲染（消毒 + 段落样式，不含段评气泡）：按正文与排版配置缓存。
// 段评索引异步到达、开关搜索等任一变化都应只触发便宜的装饰层。
export function buildBaseChapterHtml(rawText: string, opts: BaseRenderOptions) {
  if (!rawText) return ''
  const text = rawText
  const stripLeadingIndent = (line: string) => line.replace(/^[\u3000\u00A0 \t]+/, '')
  const wrapper = document.createElement('div')

  if (/<[a-z][\s\S]*>/i.test(text)) {
    // 书源返回的正文 HTML 完全不可信：先白名单消毒再进 DOM
    wrapper.innerHTML = sanitizeUntrustedHtml(text)
    if (!opts.showChapterImages) {
      wrapper.querySelectorAll('img').forEach((image) => image.remove())
    }
    const paragraphs = Array.from(wrapper.querySelectorAll('p')) as HTMLParagraphElement[]
    if (paragraphs.length) {
      paragraphs.forEach((paragraph) => {
        const plainText = (paragraph.textContent || '').replace(/^[\u3000\u00A0 \t]+/, '').trim()
        const hasRenderableChildren = Boolean(
          paragraph.querySelector('img, br, ruby, table, ul, ol')
        )
        if (!plainText && !hasRenderableChildren) {
          paragraph.remove()
          return
        }
        paragraph.innerHTML = paragraph.innerHTML.replace(/^[\u3000\u00A0 \t]+/, '')
        paragraph.style.marginTop = '0'
        paragraph.style.marginBottom = `${opts.paragraphSpacing}em`
        // 图独占一段时不缩进：首行缩进会把整张图往右推
        const imageOnly = Boolean(paragraph.querySelector('img')) && !plainText
        paragraph.classList.toggle('reader-indent', opts.firstLineIndent && !imageOnly)
      })
    }
  } else {
    wrapper.innerHTML = text
      .split(/\n/)
      .filter((line: string) => line.trim())
      .map((line: string) => {
        const shouldIndent = opts.firstLineIndent
        const content = escapeHtmlText(stripLeadingIndent(line.trimEnd()))
        return `<p${shouldIndent ? ' class="reader-indent"' : ''} style="margin-top: 0; margin-bottom: ${opts.paragraphSpacing}em;">${content}</p>`
      })
      .join('')
  }
  return wrapper.innerHTML
}

export function renderParaCommentBubble(item: ParaReviewCount) {
  const count = item.count > 999 ? '999+' : String(item.count)
  // 作者评论/点赞过的段落：气泡加底色，计数后带一支羽毛笔标记（参照番茄的样式）
  const authorMark = item.authorCommented
    ? '<svg class="para-author-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">' +
      '<path d="M20.24 12.24a6 6 0 0 0-8.49-8.49L5 10.5V19h8.5z" />' +
      '<path d="m16 8-14 14" />' +
      '<path d="M17.5 15H9" />' +
      '</svg>'
    : ''
  const title = item.authorCommented ? ' title="作者评论过这一段"' : ''
  return (
    `<span class="para-comment-bubble${item.authorCommented ? ' has-author' : ''}" role="button" tabindex="0" data-para-index="${item.paraIndex}"${title}>` +
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">' +
    '<path d="M21 11.5a8.5 8.5 0 0 1-12.3 7.6L3 21l1.9-5.6A8.5 8.5 0 1 1 21 11.5z" />' +
    '</svg>' +
    `<span class="para-comment-count">${count}</span>` +
    authorMark +
    '</span>'
  )
}

/**
 * 把配图插进正文。
 *
 * 位置是「插在该段之前」；位置缺失或超出正文范围的一律排在章末。
 * 说明文字只在正文里没有同一句话时才显示——番茄把说明也写进了正文，
 * 再显示一遍就是重复。
 */
export function insertChapterImages(wrapper: HTMLElement, imageMap: Map<number, ChapterImage[]>) {
  if (!imageMap.size) return
  const paragraphs = Array.from(wrapper.querySelectorAll('p')) as HTMLElement[]
  const trailing: ChapterImage[] = []
  imageMap.forEach((images, position) => {
    if (position < 0 || position >= paragraphs.length) {
      trailing.push(...images)
      return
    }
    const paragraph = paragraphs[position]
    const paragraphText = (paragraph.textContent || '').trim()
    paragraph.insertAdjacentHTML('beforebegin', renderChapterImageFigures(images, paragraphText))
  })
  if (trailing.length) {
    wrapper.insertAdjacentHTML('beforeend', renderChapterImageFigures(trailing, ''))
  }
}

export function renderChapterImageFigures(images: ChapterImage[], paragraphText: string) {
  return images
    .map((image) => {
      const caption = image.caption.trim()
      const size =
        image.width > 0 && image.height > 0
          ? ` width="${image.width}" height="${image.height}"`
          : ''
      const captionHtml =
        caption && caption !== paragraphText
          ? `<figcaption>${escapeHtmlText(caption)}</figcaption>`
          : ''
      // 后端把配图收口成 /reader3/image/<id>：<img> 带不了请求头，令牌只能进查询串
      const src = withAuthQuery(image.url)
      return (
        '<figure class="chapter-figure">' +
        `<img src="${escapeHtmlAttr(src)}" alt="${escapeHtmlAttr(caption)}"` +
        `${size} loading="lazy" referrerpolicy="no-referrer">` +
        `${captionHtml}</figure>`
      )
    })
    .join('')
}

/** 本地 EPUB 的 <img> 资源地址补令牌（<img> 带不了请求头）。 */
export function appendLocalEpubAssetAuth(root: HTMLElement) {
  const images = Array.from(root.querySelectorAll('img')) as HTMLImageElement[]
  images.forEach((image) => {
    const src = image.getAttribute('src') || ''
    if (src.startsWith('/reader3/localEpubAsset')) {
      image.setAttribute('src', withAuthQuery(src))
    }
  })
}

/** 搜索关键字高亮：空 query 直接跳过。 */
export function highlightSearchText(root: HTMLElement, query: string) {
  if (!query) return
  const regex = new RegExp(escapeRegExp(query), 'gi')
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement
      if (!parent || ['MARK', 'SCRIPT', 'STYLE'].includes(parent.tagName)) {
        return NodeFilter.FILTER_REJECT
      }
      regex.lastIndex = 0
      return regex.test(node.textContent || '')
        ? NodeFilter.FILTER_ACCEPT
        : NodeFilter.FILTER_REJECT
    },
  })
  const nodes: Text[] = []
  while (walker.nextNode()) {
    nodes.push(walker.currentNode as Text)
  }
  nodes.forEach((node) => {
    const value = node.textContent || ''
    regex.lastIndex = 0
    const fragment = document.createDocumentFragment()
    let cursor = 0
    for (const match of value.matchAll(regex)) {
      const index = match.index ?? 0
      if (index > cursor) {
        fragment.appendChild(document.createTextNode(value.slice(cursor, index)))
      }
      const mark = document.createElement('mark')
      mark.className = 'search-highlight'
      mark.textContent = match[0]
      fragment.appendChild(mark)
      cursor = index + match[0].length
    }
    if (cursor < value.length) {
      fragment.appendChild(document.createTextNode(value.slice(cursor)))
    }
    node.parentNode?.replaceChild(fragment, node)
  })
}
