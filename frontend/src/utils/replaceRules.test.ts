import { describe, expect, it, vi } from 'vitest'
import { applyReplaceRulesToText, matchReplaceRuleScope } from './replaceRules'
import type { Book, ReplaceRule } from '../types'

function rule(patch: Partial<ReplaceRule> = {}): ReplaceRule {
  return {
    id: 1,
    name: '规则',
    pattern: '广告',
    replacement: '',
    isEnabled: true,
    isRegex: false,
    order: 1,
    ...patch,
  }
}

const book: Book = {
  name: '示例书',
  author: '作者',
  bookId: 'book_1',
  bookUrl: 'https://example.org/book',
  origin: 'https://example.org',
  bookSourceId: 7,
}

describe('matchReplaceRuleScope', () => {
  it('空 scope 与 * 全局生效', () => {
    expect(matchReplaceRuleScope(rule({ scope: '' }), null)).toBe(true)
    expect(matchReplaceRuleScope(rule({ scope: ' * ' }), null)).toBe(true)
    expect(matchReplaceRuleScope(rule({ scope: undefined }), book)).toBe(true)
  })

  it('非全局 scope 在没有当前书时不生效', () => {
    expect(matchReplaceRuleScope(rule({ scope: 'source:7' }), null)).toBe(false)
    expect(matchReplaceRuleScope(rule({ scope: 'book:book_1' }), null)).toBe(false)
    expect(matchReplaceRuleScope(rule({ scope: '示例书' }), null)).toBe(false)
  })

  it('source: 按 bookSourceId 匹配', () => {
    expect(matchReplaceRuleScope(rule({ scope: 'source:7' }), book)).toBe(true)
    expect(matchReplaceRuleScope(rule({ scope: 'source:8' }), book)).toBe(false)
  })

  it('book: 按 bookId 匹配', () => {
    expect(matchReplaceRuleScope(rule({ scope: 'book:book_1' }), book)).toBe(true)
    expect(matchReplaceRuleScope(rule({ scope: 'book:book_2' }), book)).toBe(false)
  })

  it('旧版「书名;bookId」写法：书名命中后可再限书号', () => {
    expect(matchReplaceRuleScope(rule({ scope: '示例书' }), book)).toBe(true)
    expect(matchReplaceRuleScope(rule({ scope: '别的书' }), book)).toBe(false)
    expect(matchReplaceRuleScope(rule({ scope: '示例书;book_1' }), book)).toBe(true)
    expect(matchReplaceRuleScope(rule({ scope: '示例书;book_2' }), book)).toBe(false)
    expect(matchReplaceRuleScope(rule({ scope: '*;book_1' }), book)).toBe(true)
  })
})

describe('applyReplaceRulesToText', () => {
  it('空正文返回空串', () => {
    expect(applyReplaceRulesToText('', [rule()], book)).toBe('')
  })

  it('按 order 升序应用启用且 scope 命中的规则', () => {
    const rules = [
      rule({ id: 3, order: 3, pattern: '丙', replacement: 'C' }),
      rule({ id: 1, order: 1, pattern: '甲', replacement: 'A' }),
      rule({ id: 2, order: 2, pattern: '乙', replacement: 'B', isEnabled: false }),
      // scope 指向别的书：不命中，「示例」原样保留
      rule({ id: 4, order: 0, pattern: '示例', replacement: '', scope: 'book:book_9' }),
    ]
    expect(applyReplaceRulesToText('示例甲乙丙', rules, book)).toBe('示例A乙C')
  })

  it('正则规则带 gm 标志全局替换；非法正则不炸且不影响后续规则', () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {})
    const rules = [
      rule({ id: 1, isRegex: true, pattern: '\\d+', replacement: '#' }),
      rule({ id: 2, isRegex: true, pattern: '([', replacement: 'x' }),
      rule({ id: 3, pattern: '尾', replacement: '完' }),
    ]
    expect(applyReplaceRulesToText('第1章 第22节 尾', rules, book)).toBe('第#章 第#节 完')
    expect(errorSpy).toHaveBeenCalledOnce()
    errorSpy.mockRestore()
  })
})
