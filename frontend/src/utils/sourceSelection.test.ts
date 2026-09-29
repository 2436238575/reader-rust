import { describe, expect, it } from 'vitest'
import {
  filterBookSources,
  getBookSourceGroups,
  getBookSourceOverview,
  getBookSourceStats,
  getVisibleSelection,
  splitBookSourceGroups,
  toBookSourceDeletePayload,
} from './sourceSelection'
import type { BookSource } from '../types'

describe('sourceSelection', () => {
  const bookSources: BookSource[] = [
    { bookSourceName: 'Alpha', bookSourceUrl: 'https://alpha.example', enabled: true },
    { bookSourceName: 'Beta', bookSourceUrl: 'https://beta.example', enabled: true },
  ]

  it('keeps bulk selection scoped to currently visible source urls', () => {
    const selected = new Set(['https://alpha.example', 'https://hidden.example'])

    expect(getVisibleSelection(bookSources, selected, (source) => source.bookSourceUrl)).toEqual([
      bookSources[0],
    ])
  })

  it('builds batch delete payloads using stable source identifiers', () => {
    expect(toBookSourceDeletePayload(bookSources)).toEqual([
      { bookSourceUrl: 'https://alpha.example' },
      { bookSourceUrl: 'https://beta.example' },
    ])
  })

  it('extracts sorted groups from comma-like source group separators', () => {
    const grouped: BookSource[] = [
      { bookSourceName: 'A', bookSourceUrl: 'a', bookSourceGroup: '小说, API' },
      { bookSourceName: 'B', bookSourceUrl: 'b', bookSourceGroup: 'API；精选、小说' },
      { bookSourceName: 'C', bookSourceUrl: 'c' },
    ]

    expect(getBookSourceGroups(grouped)).toEqual(['API', '小说', '精选'])
  })

  it('splits groups on the full separator set consistently across pages', () => {
    // 全角逗号、竖线、斜杠同样是分隔符——搜索页与管理页必须拆出同一组结果
    expect(splitBookSourceGroups('小说，精选|完本/出版；API')).toEqual([
      '小说',
      '精选',
      '完本',
      '出版',
      'API',
    ])
    expect(splitBookSourceGroups(undefined)).toEqual([])
    expect(splitBookSourceGroups('  ')).toEqual([])
  })

  it('filters book sources by text and group', () => {
    const grouped: BookSource[] = [
      {
        bookSourceName: '猫眼看书',
        bookSourceUrl: 'https://maoyan.example',
        bookSourceGroup: 'API',
      },
      { bookSourceName: '笔趣阁', bookSourceUrl: 'https://biqu.example', bookSourceGroup: '网页' },
    ]

    expect(filterBookSources(grouped, 'mao', '')).toEqual([grouped[0]])
    expect(filterBookSources(grouped, '', '网页')).toEqual([grouped[1]])
  })

  it('matches filter groups exactly instead of by substring', () => {
    const grouped: BookSource[] = [
      { bookSourceName: 'A', bookSourceUrl: 'a', bookSourceGroup: '新发现' },
      { bookSourceName: 'B', bookSourceUrl: 'b', bookSourceGroup: '新' },
    ]

    expect(filterBookSources(grouped, '', '新')).toEqual([grouped[1]])
  })

  it('summarizes source counts and selected source metadata', () => {
    const list: BookSource[] = [
      {
        bookSourceName: 'Enabled',
        bookSourceUrl: 'enabled',
        enabled: true,
        ruleSearch: {},
        ruleToc: {},
        ruleReview: {},
      },
      { bookSourceName: 'Disabled', bookSourceUrl: 'disabled', enabled: false },
    ]

    expect(getBookSourceStats(list, [list[0]])).toEqual({
      total: 2,
      enabled: 1,
      filtered: 1,
    })
    expect(getBookSourceOverview(list[0])).toMatchObject({
      group: '未分组',
      statusText: '启用',
      hasSearch: true,
      hasToc: true,
      hasReview: true,
    })
    // 段评规则单独存在时也算「有评论」
    expect(
      getBookSourceOverview({
        bookSourceName: 'ParaOnly',
        bookSourceUrl: 'para',
        ruleParaReview: {},
      })
    ).toMatchObject({ hasReview: true })
    expect(getBookSourceOverview(list[1])).toMatchObject({ hasReview: false })
  })
})
