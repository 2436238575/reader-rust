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
    expect(
      toBookSourceDeletePayload([{ bookSourceId: 1 }, { bookSourceId: 2 }])
    ).toEqual([{ bookSourceId: 1 }, { bookSourceId: 2 }])
  })

  it('extracts sorted groups from comma-like source group separators', () => {
    const grouped: BookSource[] = [
      { bookSourceName: 'A', bookSourceUrl: 'a', bookSourceGroup: '小说, API' },
      { bookSourceName: 'B', bookSourceUrl: 'b', bookSourceGroup: 'API；精选、小说' },
      { bookSourceName: 'C', bookSourceUrl: 'c' },
    ]

    expect(getBookSourceGroups(grouped)).toEqual(['API', '小说', '精选'])
  })

  it('splits groups with the same separator set as the backend', () => {
    // 与后端 split_source_groups（src/service/book_source_service.rs）同口径：
    // 只认半角逗号/分号、全角分号、顿号——搜索页与管理页必须拆出同一组结果，
    // 「删除失效书源」的本地移除才与后端删除范围一致
    expect(splitBookSourceGroups('默认;失效')).toEqual(['默认', '失效'])
    expect(splitBookSourceGroups('默认；失效')).toEqual(['默认', '失效'])
    expect(splitBookSourceGroups('默认、失效')).toEqual(['默认', '失效'])
    // 全角逗号、竖线、斜杠都不是分隔符：后端不拆，前端也不得拆
    expect(splitBookSourceGroups('默认，失效')).toEqual(['默认，失效'])
    expect(splitBookSourceGroups('默认|失效')).toEqual(['默认|失效'])
    expect(splitBookSourceGroups('完本/出版')).toEqual(['完本/出版'])
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
