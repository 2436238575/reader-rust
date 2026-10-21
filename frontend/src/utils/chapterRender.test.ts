// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { applyUnderlineMarks, highlightKeywords, highlightSearchText } from './chapterRender'

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
  it('摘录命中处包 span.reader-underline，原文不变', () => {
    const el = root('<p>他走进了那座古庙，灯还亮着。</p>')
    applyUnderlineMarks(el, ['那座古庙'])
    const span = el.querySelector('span.reader-underline')
    expect(span).not.toBeNull()
    expect(span!.textContent).toBe('那座古庙')
    expect(el.textContent).toBe('他走进了那座古庙，灯还亮着。')
  })

  it('同一摘录重复出现只标第一处；多条摘录各标各的', () => {
    const el = root('<p>甲来了。乙走了。甲又来了。</p>')
    applyUnderlineMarks(el, ['甲来', '乙走'])
    const spans = el.querySelectorAll('span.reader-underline')
    expect(spans).toHaveLength(2)
    expect(spans[0].textContent).toBe('甲来')
    expect(spans[1].textContent).toBe('乙走')
  })

  it('空白数量不一致也能命中（空白段放宽匹配），不命中不动 DOM', () => {
    const el = root('<p>甲  乙　丙</p><p>无关</p>')
    applyUnderlineMarks(el, ['甲 乙 丙', '找不到的词'])
    expect(el.querySelectorAll('span.reader-underline')).toHaveLength(1)
    expect(el.textContent).toBe('甲  乙　丙无关')
  })

  it('太短（<2 字符）的摘录忽略；不进 SCRIPT/MARK 内部', () => {
    const el = root('<p>甲乙</p><style>.甲{}</style>')
    applyUnderlineMarks(el, ['甲', '乙'])
    expect(el.querySelectorAll('span.reader-underline')).toHaveLength(0)
  })

  it('划线 span 内的文本仍可被搜索/关键词标记（叠加共存）', () => {
    const el = root('<p>他走进了那座古庙。</p>')
    applyUnderlineMarks(el, ['那座古庙'])
    highlightSearchText(el, '古庙')
    const span = el.querySelector('span.reader-underline')
    expect(span?.querySelector('mark.search-highlight')?.textContent).toBe('古庙')
  })
})
