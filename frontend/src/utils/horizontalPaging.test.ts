// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import {
  adjustFitCountForReadableStart,
  appendBubbleToBlock,
  appendTextToLastParagraph,
  buildInlineStyle,
  buildParagraphHtml,
  isPunctuationOnlyText,
  mergeOrphanPunctuationPages,
  normalizeParagraphHtml,
  parseInlineStyle,
  prependTextToFirstParagraph,
  removeIndentClass,
  stripParaCommentBubble,
} from './horizontalPaging'

describe('horizontalPaging 纯函数', () => {
  it('parseInlineStyle/buildInlineStyle roundtrip', () => {
    const parsed = parseInlineStyle('text-indent: 2em; margin-bottom: 0.5em')
    expect(parsed).toEqual({ 'text-indent': '2em', 'margin-bottom': '0.5em' })
    expect(buildInlineStyle(parsed)).toBe('text-indent: 2em; margin-bottom: 0.5em')
    // 畸形项被丢弃而不是产出 undefined 键
    expect(parseInlineStyle('bad;;x:')).toEqual({})
  })

  it('removeIndentClass 只去 reader-indent，保留其余类', () => {
    expect(removeIndentClass('reader-indent foo bar')).toBe('foo bar')
    expect(removeIndentClass('foo')).toBe('foo')
    expect(removeIndentClass('')).toBe('')
  })

  it('normalizeParagraphHtml 给 reader-indent 补 text-indent，已有 style 不覆盖', () => {
    const withClass = normalizeParagraphHtml('<p class="reader-indent">正文</p>')
    expect(withClass).toContain('text-indent: 2em')
    const already = normalizeParagraphHtml(
      '<p class="reader-indent" style="text-indent: 3em">正文</p>'
    )
    expect(already).toContain('text-indent: 3em')
    expect(already).not.toContain('2em')
  })

  it('buildParagraphHtml 转义正文里的 HTML 特殊字符', () => {
    expect(buildParagraphHtml('', 'a<b>&"')).toBe('<p>a&lt;b&gt;&amp;"</p>')
    expect(buildParagraphHtml('text-indent: 2em', 'x', 'cls')).toBe(
      '<p class="cls" style="text-indent: 2em">x</p>'
    )
  })

  it('isPunctuationOnlyText 只认短标点串', () => {
    expect(isPunctuationOnlyText('。')).toBe(true)
    expect(isPunctuationOnlyText('……')).toBe(true)
    expect(isPunctuationOnlyText('，')).toBe(true)
    expect(isPunctuationOnlyText('正文')).toBe(false)
    expect(isPunctuationOnlyText('。一些字')).toBe(false)
    expect(isPunctuationOnlyText('。。。。。')).toBe(false) // 超 3 字符
    expect(isPunctuationOnlyText('')).toBe(false)
  })

  it('adjustFitCountForReadableStart 回溯避开标点开头', () => {
    const text = '他说了很多话，然后转身离开。夜色渐深'
    // 下标：他0 说1 了2 很3 多4 话5 ，6 然7 后8 转9 身10 离11 开12 。13 夜14
    // fit=13 时下一页以「。」开头 → 回溯到最近分断点（逗号后，返回 7）
    const adjusted = adjustFitCountForReadableStart(text, 13)
    expect(adjusted).toBe(7)
    expect(text.slice(adjusted).charAt(0)).toBe('然')
    // 下一页开头不是标点时原样返回
    expect(adjustFitCountForReadableStart(text, 7)).toBe(7)
    // 越界入参不动
    expect(adjustFitCountForReadableStart(text, 0)).toBe(0)
    expect(adjustFitCountForReadableStart(text, text.length)).toBe(text.length)
  })

  it('stripParaCommentBubble/appendBubbleToBlock 摘下再挂回', () => {
    const block =
      '<p style="text-indent: 2em">正文内容</p><span class="para-comment-bubble">12</span>'
    const { html, bubbleHtml } = stripParaCommentBubble(block)
    expect(html).not.toContain('para-comment-bubble')
    expect(bubbleHtml).toContain('para-comment-bubble')
    const restored = appendBubbleToBlock(html, bubbleHtml)
    expect(restored).toContain('正文内容')
    expect(restored).toContain('para-comment-bubble')
    // 气泡挂回段落闭合标签之前
    expect(restored.indexOf('para-comment-bubble')).toBeLessThan(restored.lastIndexOf('</p>'))
    // 无气泡的块原样返回
    const plain = stripParaCommentBubble('<p>无气泡</p>')
    expect(plain.bubbleHtml).toBe('')
    expect(plain.html).toBe('<p>无气泡</p>')
  })

  it('mergeOrphanPunctuationPages 孤标点页并入前一页', () => {
    const pages = ['<p>第一页正文</p>', '<p>。</p>', '<p>第二页正文</p>']
    const merged = mergeOrphanPunctuationPages(pages)
    expect(merged).toHaveLength(2)
    expect(merged[0]).toContain('第一页正文。')
    expect(merged[1]).toContain('第二页正文')
  })

  it('mergeOrphanPunctuationPages 首页即孤标点时前移到下一页', () => {
    const pages = ['<p>，</p>', '<p>正文页</p>']
    const merged = mergeOrphanPunctuationPages(pages)
    expect(merged).toHaveLength(1)
    expect(merged[0]).toContain('，正文页')
  })

  it('mergeOrphanPunctuationPages 正常页不受影响', () => {
    const pages = ['<p>甲</p>', '<p>乙</p>']
    expect(mergeOrphanPunctuationPages(pages)).toEqual(pages)
  })

  it('prepend/appendText 在无段落时降级为转义文本拼接', () => {
    expect(prependTextToFirstParagraph('<div>x</div>', 'a<b')).toBe('a&lt;b<div>x</div>')
    expect(appendTextToLastParagraph('<div>x</div>', 'c')).toBe('<div>x</div>c')
  })
})
