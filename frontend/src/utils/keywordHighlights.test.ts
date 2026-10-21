import { describe, expect, it } from 'vitest'
import {
  darkenKeywordColor,
  mergeKeywordRules,
  newKeywordRuleId,
  sanitizeKeywordRules,
  type KeywordHighlightRule,
} from './keywordHighlights'

function rule(patch: Partial<KeywordHighlightRule> = {}): KeywordHighlightRule {
  return {
    id: 'kw-1',
    keyword: '关键词',
    color: '#ffd54a80',
    enabled: true,
    scope: 'global',
    updatedAt: 100,
    ...patch,
  }
}

describe('sanitizeKeywordRules', () => {
  it('丢掉形状不对的条目，保留合法字段', () => {
    const cleaned = sanitizeKeywordRules([
      rule(),
      null,
      'x',
      { id: '', keyword: 'a' },
      { id: 'kw-2', keyword: '  ' },
      { id: 'kw-3', keyword: '甲', color: '', enabled: false, scope: 'book', bookId: 'book_7' },
    ])
    expect(cleaned).toHaveLength(2)
    expect(cleaned[0].id).toBe('kw-1')
    // color 缺省回默认色；scope 只认 book，其余归 global
    expect(cleaned[1].enabled).toBe(false)
    expect(cleaned[1].scope).toBe('book')
    expect(cleaned[1].bookId).toBe('book_7')
    expect(cleaned[1].color).toBeTruthy()
  })

  it('非数组输入一律回空', () => {
    expect(sanitizeKeywordRules(null)).toEqual([])
    expect(sanitizeKeywordRules({})).toEqual([])
    expect(sanitizeKeywordRules('[]')).toEqual([])
  })
})

describe('mergeKeywordRules', () => {
  it('按 id 并集，冲突取 updatedAt 较新的一版', () => {
    const local = [rule({ id: 'a', keyword: '旧', updatedAt: 100 }), rule({ id: 'b' })]
    const remote = [rule({ id: 'a', keyword: '新', updatedAt: 200 }), rule({ id: 'c' })]
    const merged = mergeKeywordRules(local, remote)
    expect(merged).toHaveLength(3)
    expect(merged.find((r) => r.id === 'a')?.keyword).toBe('新')
  })

  it('双端各自独有的条目都保留', () => {
    const merged = mergeKeywordRules([rule({ id: 'a' })], [rule({ id: 'b' })])
    expect(merged.map((r) => r.id).sort()).toEqual(['a', 'b'])
  })
})

describe('newKeywordRuleId', () => {
  it('两次生成不重复', () => {
    expect(newKeywordRuleId()).not.toBe(newKeywordRuleId())
  })
})

describe('darkenKeywordColor', () => {
  it('半透明浅色压暗成同色相深色底，不透明通道抬高', () => {
    // #ffd54a80 = rgb(255,213,74) alpha 0.5
    expect(darkenKeywordColor('#ffd54a80')).toBe('rgba(107, 89, 31, 0.70)')
  })

  it('不带透明通道的六位 hex 按不透明处理', () => {
    expect(darkenKeywordColor('#ffd54a')).toBe('rgba(107, 89, 31, 0.75)')
  })

  it('无法解析的原样返回', () => {
    expect(darkenKeywordColor('red')).toBe('red')
    expect(darkenKeywordColor('')).toBe('')
  })
})
