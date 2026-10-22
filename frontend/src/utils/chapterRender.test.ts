// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import {
  applyDiffMarks,
  applyUnderlineMarks,
  buildParaBubbleMap,
  highlightKeywords,
  highlightSearchText,
  paraTextSimilarity,
} from './chapterRender'
import type { ParagraphDiffOp } from './chapterDiff'
import type { ParaReviewCount } from '../types'

function root(html: string): HTMLElement {
  const el = document.createElement('div')
  el.innerHTML = html
  return el
}

describe('highlightKeywords', () => {
  it('命中处包 mark.kw-highlight 并上底色，未命中处不动', () => {
    const el = root('<p>张三走进了客栈，李四跟在后面。</p>')
    highlightKeywords(el, [{ keyword: '张三', color: '#ffd54a80' }])
    const mark = el.querySelector<HTMLElement>('mark.kw-highlight')
    expect(mark).not.toBeNull()
    expect(mark!.textContent).toBe('张三')
    expect(mark!.style.backgroundColor).toBeTruthy()
    expect(el.textContent).toBe('张三走进了客栈，李四跟在后面。')
  })

  it('多处命中全部标记；空关键词与空色跳过', () => {
    const el = root('<p>甲和乙，甲先走。</p>')
    highlightKeywords(el, [
      { keyword: '甲', color: '#fff' },
      { keyword: '  ', color: '#000' },
      { keyword: '乙', color: '' },
    ])
    expect(el.querySelectorAll('mark.kw-highlight')).toHaveLength(2)
  })

  it('不进 SCRIPT/STYLE 与已有 MARK 内部', () => {
    const el = root('<p>关键词</p><style>.关键词{color:red}</style>')
    highlightKeywords(el, [{ keyword: '关键词', color: '#fff' }])
    expect(el.querySelectorAll('p mark.kw-highlight')).toHaveLength(1)
    expect(el.querySelector('style')!.textContent).toContain('关键词')
  })

  it('与搜索高亮共存：搜索先注入，重叠处搜索赢', () => {
    const el = root('<p>笑傲江湖曲</p>')
    highlightSearchText(el, '笑傲')
    highlightKeywords(el, [{ keyword: '笑傲江湖', color: '#ffd54a80' }])
    // 「笑傲」已被搜索 mark 包走，关键词只能在剩余文本「江湖曲」上命中「江湖」之外的片段；
    // 重叠命中不进已有 MARK：搜索 mark 保持完整
    const searchMark = el.querySelector('mark.search-highlight')
    expect(searchMark?.textContent).toBe('笑傲')
    expect(el.querySelectorAll('mark.kw-highlight')).toHaveLength(0)
  })

  it('关键词之间按数组顺序：重叠命中先入者赢', () => {
    const el = root('<p>abcdef</p>')
    highlightKeywords(el, [
      { keyword: 'abc', color: '#111111' },
      { keyword: 'bcd', color: '#222222' },
    ])
    const marks = el.querySelectorAll('mark.kw-highlight')
    expect(marks).toHaveLength(1)
    expect(marks[0].textContent).toBe('abc')
  })

  it('关键词里的正则元字符按字面匹配（escapeRegExp）', () => {
    const el = root('<p>价格是 100 (含税)，不是 100。</p>')
    highlightKeywords(el, [{ keyword: '(含税)', color: '#fff' }])
    const marks = el.querySelectorAll('mark.kw-highlight')
    expect(marks).toHaveLength(1)
    expect(marks[0].textContent).toBe('(含税)')
  })
})

describe('applyUnderlineMarks', () => {
  const u = (text: string, chapterPos = 1000) => ({ text, chapterPos })

  it('摘录命中处包 span.reader-underline 并带 chapterPos，原文不变', () => {
    const el = root('<p>他走进了那座古庙，灯还亮着。</p>')
    applyUnderlineMarks(el, [u('那座古庙', 3456)])
    const span = el.querySelector<HTMLElement>('span.reader-underline')
    expect(span).not.toBeNull()
    expect(span!.textContent).toBe('那座古庙')
    expect(span!.dataset.chapterPos).toBe('3456')
    expect(el.textContent).toBe('他走进了那座古庙，灯还亮着。')
  })

  it('同一摘录重复出现只标第一处；多条摘录各标各的', () => {
    const el = root('<p>甲来了。乙走了。甲又来了。</p>')
    applyUnderlineMarks(el, [u('甲来'), u('乙走')])
    const spans = el.querySelectorAll('span.reader-underline')
    expect(spans).toHaveLength(2)
    expect(spans[0].textContent).toBe('甲来')
    expect(spans[1].textContent).toBe('乙走')
  })

  it('空白数量不一致也能命中（空白段放宽匹配），不命中不动 DOM', () => {
    const el = root('<p>甲  乙　丙</p><p>无关</p>')
    applyUnderlineMarks(el, [u('甲 乙 丙'), u('找不到的词')])
    expect(el.querySelectorAll('span.reader-underline')).toHaveLength(1)
    expect(el.textContent).toBe('甲  乙　丙无关')
  })

  it('太短（<2 字符）的摘录忽略；不进 SCRIPT/MARK 内部', () => {
    const el = root('<p>甲乙</p><style>.甲{}</style>')
    applyUnderlineMarks(el, [u('甲'), u('乙')])
    expect(el.querySelectorAll('span.reader-underline')).toHaveLength(0)
  })

  it('划线 span 内的文本仍可被搜索/关键词标记（叠加共存）', () => {
    const el = root('<p>他走进了那座古庙。</p>')
    applyUnderlineMarks(el, [u('那座古庙')])
    highlightSearchText(el, '古庙')
    const span = el.querySelector('span.reader-underline')
    expect(span?.querySelector('mark.search-highlight')?.textContent).toBe('古庙')
  })

  it('跨段划线按行拆开：每段各标一处，共享同一 chapterPos', () => {
    const el = root('<p>第一段的后半在这里收尾。</p><p>第二段开头接着讲。</p>')
    applyUnderlineMarks(el, [u('后半在这里收尾。\n第二段开头接着讲', 7777)])
    const spans = el.querySelectorAll<HTMLElement>('span.reader-underline')
    expect(spans).toHaveLength(2)
    expect(spans[0].textContent).toBe('后半在这里收尾。')
    expect(spans[1].textContent).toBe('第二段开头接着讲')
    expect(spans[0].dataset.chapterPos).toBe('7777')
    expect(spans[1].dataset.chapterPos).toBe('7777')
  })

  it('跨三段的长划线：中间整段也标', () => {
    const el = root('<p>开头甲段</p><p>整整一段乙</p><p>丙收尾了</p>')
    applyUnderlineMarks(el, [u('甲段\n整整一段乙\n丙收')])
    const spans = el.querySelectorAll('span.reader-underline')
    expect(spans).toHaveLength(3)
    expect(spans[1].textContent).toBe('整整一段乙')
  })
})

describe('applyDiffMarks', () => {
  const equal = (baseIndex: number, editedIndex: number): ParagraphDiffOp => ({
    type: 'equal',
    baseIndex,
    editedIndex,
  })
  const add = (editedIndex: number, editedText: string): ParagraphDiffOp => ({
    type: 'add',
    editedIndex,
    editedText,
  })
  const del = (baseIndex: number, baseText: string): ParagraphDiffOp => ({
    type: 'del',
    baseIndex,
    baseText,
  })
  const modify = (
    baseIndex: number,
    editedIndex: number,
    baseText: string,
    editedText: string
  ): ParagraphDiffOp => ({ type: 'modify', baseIndex, editedIndex, baseText, editedText })

  it('新增段打 diff-added，未变段不动', () => {
    const el = root('<p>甲段</p><p>新加的段</p><p>乙段</p>')
    applyDiffMarks(el, [equal(0, 0), add(1, '新加的段'), equal(1, 2)])
    const ps = el.querySelectorAll('p')
    expect(ps[1].classList.contains('diff-added')).toBe(true)
    expect((ps[1] as HTMLElement).dataset.diffOp).toBe('1')
    expect(ps[0].className).toBe('')
    expect(ps[2].className).toBe('')
  })

  it('段内改动：diff-modified + 增加部分虚线 span + 删除点红竖线', () => {
    // 渲染的正文是编辑版；基准里的「进」被改成「出」
    const el = root('<p>甲段</p><p>他走出了那座古庙。</p>')
    applyDiffMarks(el, [equal(0, 0), modify(1, 1, '他走进了那座古庙。', '他走出了那座古庙。')])
    const p = el.querySelectorAll('p')[1]
    expect(p.classList.contains('diff-modified')).toBe(true)
    expect((p as HTMLElement).dataset.diffOp).toBe('1')
    expect(p.querySelector('span.diff-ins')?.textContent).toBe('出')
    const marker = p.querySelector<HTMLElement>('span.diff-del-marker')
    expect(marker).not.toBeNull()
    expect(marker!.title).toContain('进')
    // 原文不变（标记只是包裹）
    expect(p.textContent).toContain('他走出了那座古庙。')
  })

  it('大段删除：删除块后第一个未变段打 diff-del-after；文末删除标最后一段', () => {
    const el = root('<p>甲段</p><p>乙段</p>')
    // 基准 [甲,删1,删2,乙]，编辑版 [甲,乙]：删除块夹在中间
    applyDiffMarks(el, [equal(0, 0), del(1, '删一'), del(2, '删二'), equal(3, 1)])
    const ps = el.querySelectorAll('p')
    expect(ps[1].classList.contains('diff-del-after')).toBe(true)
    expect((ps[1] as HTMLElement).dataset.diffDelStart).toBe('1')
    expect(ps[0].classList.contains('diff-del-after')).toBe(false)

    const el2 = root('<p>甲段</p><p>乙段</p>')
    applyDiffMarks(el2, [equal(0, 0), equal(1, 1), del(2, '删尾')])
    const ps2 = el2.querySelectorAll('p')
    expect(ps2[1].classList.contains('diff-del-after')).toBe(true)
    expect((ps2[1] as HTMLElement).dataset.diffDelStart).toBe('2')
  })

  it('diff 标记后划线/搜索仍能穿过 span 匹配', () => {
    const el = root('<p>他走出了那座古庙。</p>')
    applyDiffMarks(el, [modify(0, 0, '他走进了那座古庙。', '他走出了那座古庙。')])
    highlightSearchText(el, '古庙')
    expect(el.querySelector('mark.search-highlight')?.textContent).toBe('古庙')
  })
})

describe('段评定位', () => {
  const count = (paraIndex: number, text: string): ParaReviewCount => ({
    paraIndex,
    count: 1,
    authorCommented: false,
    text,
  })

  it('普通章节按行号映射', () => {
    const lines = [
      { index: 0, text: '甲段' },
      { index: 2, text: '乙段' },
    ]
    const byPosition = new Map([
      [0, 0],
      [1, 2],
    ])
    const map = buildParaBubbleMap([count(2, '乙段')], lines, byPosition, false)
    expect(map.get(1)?.paraIndex).toBe(2)
  })

  it('编辑版：段前插入新内容后按原文找到原段', () => {
    // 上游第一段是「男主走进客栈」，编辑版在前面插了一段，行号整体后移
    const lines = [
      { index: 0, text: '新增的前言。' },
      { index: 1, text: '男主走进客栈，四下张望。' },
      { index: 2, text: '反派系统又响了。' },
    ]
    const byPosition = new Map([
      [0, 0],
      [1, 1],
      [2, 2],
    ])
    const map = buildParaBubbleMap(
      [count(0, '男主走进客栈，四下张望。'), count(1, '反派系统又响了。')],
      lines,
      byPosition,
      true
    )
    expect(map.get(1)?.paraIndex).toBe(0)
    expect(map.get(2)?.paraIndex).toBe(1)
  })

  it('编辑版：改了几个词仍能匹配', () => {
    const lines = [{ index: 0, text: '反派系统再次响了，声音很急。' }]
    const byPosition = new Map([[0, 0]])
    const map = buildParaBubbleMap(
      [count(0, '反派系统又响了，声音很急。')],
      lines,
      byPosition,
      true
    )
    expect(map.get(0)?.paraIndex).toBe(0)
  })

  it('编辑版：整段被删后不再挂气泡', () => {
    const lines = [{ index: 0, text: '剩下的一段。' }]
    const byPosition = new Map([[0, 0]])
    const map = buildParaBubbleMap([count(0, '这一段已经被用户删掉了。')], lines, byPosition, true)
    expect(map.size).toBe(0)
  })

  it('相似度：短段按前缀，长段按二元组', () => {
    expect(paraTextSimilarity('……', '……')).toBe(1)
    expect(paraTextSimilarity('二', '二改')).toBe(1)
    expect(paraTextSimilarity('甲', '乙')).toBe(0)
    expect(
      paraTextSimilarity('今天天气很好，我们一起去公园。', '今天天气不错，我们一起去公园。')
    ).toBeGreaterThan(0.5)
    expect(paraTextSimilarity('今天天气很好，我们一起去公园。', '系统提示任务完成。')).toBeLessThan(
      0.5
    )
  })
})
