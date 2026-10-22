// 阅读器正文渲染的纯函数簇（从 ReaderView.vue 抽出）：
// 消毒/段落样式的基础渲染、章节配图插入、段评气泡、EPUB 资源改写、搜索高亮。
// 全部是「输入 → DOM/HTML 字符串」的确定性函数，不持有响应式状态。

import { withAuthQuery } from '../api/bookshelf'
import { sanitizeUntrustedHtml } from './sanitize'
import type { ChapterImage, ParaReviewCount } from '../types'
import { inlineDiff, type ParagraphDiffOp } from './chapterDiff'

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

function normalizeDiffText(text: string) {
  return text
    .replace(/<[^>]+>/g, '')
    .replace(/\s+/g, '')
    .trim()
}

/**
 * 把 diff 操作对到渲染后的段落上。
 *
 * 后端按**原始正文**算 diff，而渲染前还会跑替换规则（可能删行）与繁简转换。
 * 段落数没变时下标仍然可靠；变了就按 `editedText` 顺序找原文，找不到的
 * 操作宁可不标，也不把标记贴到错误段落上。
 */
function resolveDiffParagraphs(paragraphs: HTMLElement[], ops: ParagraphDiffOp[]) {
  const resolved = new Map<number, HTMLElement>()
  const used = new Set<number>()
  let cursor = 0
  let maxEditedIndex = -1

  for (let opIndex = 0; opIndex < ops.length; opIndex += 1) {
    const op = ops[opIndex]
    if (op.type === 'del' || op.editedIndex == null) continue
    maxEditedIndex = Math.max(maxEditedIndex, op.editedIndex)
    const expected = normalizeDiffText(op.editedText || '')
    if (!expected) continue

    const direct = paragraphs[op.editedIndex]
    if (
      direct &&
      !used.has(op.editedIndex) &&
      normalizeDiffText(direct.textContent || '') === expected
    ) {
      resolved.set(opIndex, direct)
      used.add(op.editedIndex)
      cursor = Math.max(cursor, op.editedIndex + 1)
      continue
    }

    // 只在当前位置附近找：替换规则删/增行时下标会漂，但不会把整章重排
    const limit = Math.min(paragraphs.length, cursor + 64)
    let bestIndex = -1
    let bestScore = 0
    for (let index = cursor; index < limit; index += 1) {
      if (used.has(index)) continue
      const candidate = normalizeDiffText(paragraphs[index].textContent || '')
      if (!candidate) continue
      if (candidate === expected) {
        bestIndex = index
        bestScore = 1
        break
      }
      const score = paraTextSimilarity(expected, candidate)
      if (score > bestScore) {
        bestScore = score
        bestIndex = index
      }
    }
    if (bestIndex >= 0 && bestScore >= 0.75) {
      resolved.set(opIndex, paragraphs[bestIndex])
      used.add(bestIndex)
      cursor = bestIndex + 1
    }
  }

  // 段落数一致（没有规则增删行）：文本对不上（繁简转换等）也按原下标兜底
  if (paragraphs.length === maxEditedIndex + 1) {
    for (let opIndex = 0; opIndex < ops.length; opIndex += 1) {
      const op = ops[opIndex]
      if (op.type === 'del' || op.editedIndex == null || resolved.has(opIndex)) continue
      if (used.has(op.editedIndex)) continue
      const paragraph = paragraphs[op.editedIndex]
      if (paragraph) {
        resolved.set(opIndex, paragraph)
        used.add(op.editedIndex)
      }
    }
  }
  return resolved
}

/**
 * 编辑版与上游基准的 diff 标记（vscode git diff 风）：
 *
 * - 新增段：段落左绿竖线（`p.diff-added`）
 * - 段内改动：段落左蓝竖线（`p.diff-modified`）+ 相对原段增加的部分包
 *   `span.diff-ins`（虚线下划线），减少处在删除点插零宽 `span.diff-del-marker`
 *   （红竖线，title 带被删原文）
 * - 大段删除：删除块后第一个未变段打 `p.diff-del-after`，`::before` 红箭头
 *   （删除块在文末时标最后一个段）
 *
 * 在装饰层**最前**跑（applyUnderlineMarks 之前）：它改的是段级 class 与
 * 段内 span，后续划线/搜索/关键词的 TreeWalker 都能穿过 span 继续匹配。
 * 只动 class 与内联 span、不新增顶层块、不含 SVG——横翻分页器的块级
 * 切分与高度测量完全不受影响。
 */
export function applyDiffMarks(wrapper: HTMLElement, ops: ParagraphDiffOp[]) {
  if (!ops.length) return
  const paragraphs = Array.from(wrapper.querySelectorAll('p')) as HTMLElement[]
  if (!paragraphs.length) return
  const resolved = resolveDiffParagraphs(paragraphs, ops)
  // 删除块的首个 del 下标：点击红箭头时据此取出整块被删段落
  let pendingDeleteStart: number | null = null
  ops.forEach((op, opIndex) => {
    if (op.type === 'del') {
      if (pendingDeleteStart == null) pendingDeleteStart = opIndex
      return
    }
    const index = op.editedIndex
    if (index == null) return
    const paragraph = resolved.get(opIndex)
    if (!paragraph) return
    if (pendingDeleteStart != null) {
      paragraph.dataset.diffDelStart = String(pendingDeleteStart)
      paragraph.classList.add('diff-del-after')
      pendingDeleteStart = null
    }
    if (op.type === 'equal') return
    // 段落自身对应的操作：点击该段就地预览「原 → 改」
    paragraph.dataset.diffOp = String(opIndex)
    if (op.type === 'add') {
      paragraph.classList.add('diff-added')
      return
    }
    if (op.type !== 'modify') return
    paragraph.classList.add('diff-modified')
    const diff = inlineDiff(op.baseText || '', paragraph.textContent || '')
    if (!diff.removed && !diff.added) return
    const fragment = document.createDocumentFragment()
    if (diff.prefix) fragment.appendChild(document.createTextNode(diff.prefix))
    if (diff.removed) {
      const marker = document.createElement('span')
      marker.className = 'diff-del-marker'
      marker.title = `删除：${diff.removed.length > 30 ? `${diff.removed.slice(0, 30)}…` : diff.removed}`
      fragment.appendChild(marker)
    }
    if (diff.added) {
      const ins = document.createElement('span')
      ins.className = 'diff-ins'
      ins.textContent = diff.added
      fragment.appendChild(ins)
    }
    if (diff.suffix) fragment.appendChild(document.createTextNode(diff.suffix))
    paragraph.textContent = ''
    paragraph.appendChild(fragment)
  })
  // 删除块在文末：标最后一个段
  if (pendingDeleteStart != null) {
    paragraphs[paragraphs.length - 1].dataset.diffDelStart = String(pendingDeleteStart)
    paragraphs[paragraphs.length - 1].classList.add('diff-del-after')
  }
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

/* ─── 段评气泡定位 ─── */

/** 编辑版章节用段落原文匹配时，最低相似度（0–1）。 */
const PARA_TEXT_MATCH_THRESHOLD = 0.5

function normalizeParaText(text: string) {
  return text
    .replace(/<[^>]+>/g, '')
    .replace(/\s+/g, '')
    .toLowerCase()
}

/**
 * 两段文字相似度：字符二元组 Dice 系数，短段（< 4 字）用公共前缀比例。
 *
 * 段评概览里的原文被截到 60 字，编辑版段落可能更长：比较前把目标截到
 * 相近长度，避免长段落自身长度把相似度稀释掉。
 */
export function paraTextSimilarity(source: string, target: string): number {
  const left = normalizeParaText(source)
  const right = normalizeParaText(target)
  if (!left || !right) return 0
  if (left === right) return 1
  const leftChars = Array.from(left)
  const rightChars = Array.from(right)
  const shorter = Math.min(leftChars.length, rightChars.length)
  if (shorter === 0) return 0

  let prefix = 0
  while (prefix < shorter && leftChars[prefix] === rightChars[prefix]) prefix += 1
  const prefixScore = prefix / shorter
  if (shorter < 4) return prefixScore

  const rightForBigrams = rightChars.slice(0, leftChars.length + 10)
  const leftBigrams = new Map<string, number>()
  for (let i = 0; i < leftChars.length - 1; i += 1) {
    const key = `${leftChars[i]}${leftChars[i + 1]}`
    leftBigrams.set(key, (leftBigrams.get(key) ?? 0) + 1)
  }
  let shared = 0
  for (let i = 0; i < rightForBigrams.length - 1; i += 1) {
    const key = `${rightForBigrams[i]}${rightForBigrams[i + 1]}`
    const leftCount = leftBigrams.get(key)
    if (leftCount) {
      shared += 1
      if (leftCount === 1) leftBigrams.delete(key)
      else leftBigrams.set(key, leftCount - 1)
    }
  }
  const dice = (2 * shared) / (leftChars.length - 1 + rightForBigrams.length - 1)
  return Math.max(prefixScore, dice)
}

/**
 * 段号 → 渲染段落位置。
 *
 * 普通章节：段评的 `paraIndex` 就是正文行号，按「渲染位置 ↔ 原始行号」
 * 映射直接取。
 *
 * 编辑过的章节：`paraIndex` 指向上游正文，编辑增删段落后行号会漂移。
 * 改用段评概览自带的 `text`（该段原文）去编辑版正文里做相似度匹配；
 * 匹配不上说明这段已经被删掉，不挂气泡。
 */
export function buildParaBubbleMap(
  counts: ParaReviewCount[],
  rawLines: { index: number; text: string }[],
  rawLineByPosition: Map<number, number>,
  edited: boolean
): Map<number, ParaReviewCount> {
  const map = new Map<number, ParaReviewCount>()
  if (!counts.length) return map
  const positionByRawLine = new Map<number, number>()
  rawLineByPosition.forEach((rawIndex, position) => {
    if (!positionByRawLine.has(rawIndex)) positionByRawLine.set(rawIndex, position)
  })

  if (!edited) {
    for (const item of counts) {
      const position = positionByRawLine.get(item.paraIndex)
      if (position != null) map.set(position, item)
    }
    return map
  }

  const used = new Set<number>()
  for (const item of counts) {
    if (!normalizeParaText(item.text)) continue
    let bestRawIndex = -1
    let bestScore = 0
    for (const line of rawLines) {
      if (used.has(line.index)) continue
      const score = paraTextSimilarity(item.text, line.text)
      if (score > bestScore) {
        bestScore = score
        bestRawIndex = line.index
      }
    }
    if (bestRawIndex < 0 || bestScore < PARA_TEXT_MATCH_THRESHOLD) continue
    const position = positionByRawLine.get(bestRawIndex)
    if (position == null) continue
    used.add(bestRawIndex)
    map.set(position, item)
  }
  return map
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

export interface KeywordMarkSpec {
  keyword: string
  color: string
}

export interface UnderlineMarkSpec {
  /** 划线摘录文本（可含换行 = 跨段） */
  text: string
  /** 对应书签条目的 chapterPos（万分比进度），落在 span 上供点击定位回原条目 */
  chapterPos: number
}

/**
 * 划线回显：把划线摘录文本在正文里包成 `<span class="reader-underline">`，
 * span 带 `data-chapter-pos` 指向对应书签条目（点击/右键/长按打开操作菜单用）。
 *
 * 注入顺序要在搜索/关键词高亮**之前**：span 不在 walker 的排除名单里，
 * 后续两个高亮还能进 span 内部叠加（下划线 + 底色共存）；反过来的话
 * mark 会挡住划线匹配。
 *
 * 跨段划线：选区摘录里的换行正好对应段落边界（浏览器对跨段选区
 * `toString()` 给 `\n`），按行拆开——每行是各自段落内的连续子串，
 * 走同一个单文本节点匹配，每行标一处（共享同一个 chapterPos）。
 *
 * 已知局限：同一行摘录在一章里重复出现时只标第一处。
 */
export function applyUnderlineMarks(root: HTMLElement, marks: UnderlineMarkSpec[]) {
  // 拆行后 (行文本, chapterPos) 逐条作为独立 anchor
  const anchors = marks
    .flatMap((mark) =>
      mark.text.split(/\n+/).map((line) => ({
        text: line.replace(/\s+/g, ' ').trim(),
        chapterPos: mark.chapterPos,
      }))
    )
    .filter((anchor) => anchor.text.length >= 2)
  if (!anchors.length) return
  const marked = new Set<string>()
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement
      if (!parent || ['MARK', 'SCRIPT', 'STYLE'].includes(parent.tagName)) {
        return NodeFilter.FILTER_REJECT
      }
      return NodeFilter.FILTER_ACCEPT
    },
  })
  const nodes: Text[] = []
  while (walker.nextNode()) {
    nodes.push(walker.currentNode as Text)
  }
  for (let i = 0; i < nodes.length; i++) {
    const node = nodes[i]
    const value = node.textContent || ''
    if (!value.trim()) continue
    for (const anchor of anchors) {
      if (marked.has(anchor.text)) continue
      // 摘录的空白与正文渲染后的空白数量可能不一致，把空白段放宽成 \s+，
      // 让命中坐标直接落在原文上（不归一化原文，避免坐标错位）
      const pattern = anchor.text.split(/\s+/).map(escapeRegExp).join('\\s+')
      const match = new RegExp(pattern).exec(value)
      if (!match) continue
      const before = value.slice(0, match.index)
      const hit = match[0]
      const after = value.slice(match.index + hit.length)
      const fragment = document.createDocumentFragment()
      if (before) fragment.appendChild(document.createTextNode(before))
      const span = document.createElement('span')
      span.className = 'reader-underline'
      span.dataset.chapterPos = String(anchor.chapterPos)
      span.textContent = hit
      fragment.appendChild(span)
      const afterNode = after ? document.createTextNode(after) : null
      if (afterNode) fragment.appendChild(afterNode)
      node.parentNode?.replaceChild(fragment, node)
      marked.add(anchor.text)
      // after 段是新建文本节点、不在预收集列表里：推回队列尾部，
      // 让同节点里排在后面的其他划线也能命中
      if (afterNode) {
        nodes.push(afterNode)
      }
      break // 本条划线已标记，处理下一条节点
    }
  }
}

/**
 * 常驻关键词高亮：命中处包 `<mark class="kw-highlight">` 并上底色。
 *
 * 调用点要排在 highlightSearchText 之后：TreeWalker 不进已有 MARK 内部，
 * 搜索高亮先注入，重叠时搜索赢（用户正在找的词优先显眼）。多个关键词按
 * 数组顺序逐趟标记，先注入的关键词在重叠命中处赢。
 *
 * 已知限制（与搜索高亮一致）：横翻分页器对含内联标签的段落走字符切分
 * 重建，跨页切段里 mark 会丢，整段放进一页时正常。
 */
export function highlightKeywords(root: HTMLElement, specs: KeywordMarkSpec[]) {
  for (const spec of specs) {
    const keyword = spec.keyword.trim()
    if (!keyword || !spec.color) continue
    const regex = new RegExp(escapeRegExp(keyword), 'gi')
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
        mark.className = 'kw-highlight'
        mark.style.backgroundColor = spec.color
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
}
