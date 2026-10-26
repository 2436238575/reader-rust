import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  loadCachedEditedChapterKeys,
  readEditedChaptersCache,
  saveCachedEditedChapterKeys,
} from './editedChaptersCache'

describe('editedChaptersCache 已编辑章快照', () => {
  beforeEach(() => {
    const storage = new Map<string, string>()
    vi.stubGlobal('localStorage', {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
      clear: vi.fn(() => storage.clear()),
    })
  })

  it('写入后能按书读回章节 URL 集合', () => {
    saveCachedEditedChapterKeys('book_1', ['https://example.org/1', 'https://example.org/2'])

    expect(loadCachedEditedChapterKeys('book_1')).toEqual([
      'https://example.org/1',
      'https://example.org/2',
    ])
    expect(loadCachedEditedChapterKeys('book_2')).toEqual([])
  })

  it('损坏数据按空快照处理，不抛错', () => {
    localStorage.setItem('reader-edited-chapters', 'not-json')
    expect(readEditedChaptersCache()).toEqual({})

    localStorage.setItem('reader-edited-chapters', JSON.stringify(['array']))
    expect(loadCachedEditedChapterKeys('book_1')).toEqual([])

    // urls 里的非字符串条目被过滤
    localStorage.setItem(
      'reader-edited-chapters',
      JSON.stringify({ book_1: { urls: ['a', 1, null, 'b'], savedAt: 1 } })
    )
    expect(loadCachedEditedChapterKeys('book_1')).toEqual(['a', 'b'])
  })

  it('最多保留 20 本书，最旧的被淘汰', () => {
    // savedAt 用单调递增的假时间：真实 Date.now 在快速循环里会同毫秒，排不出新旧
    let now = 0
    vi.spyOn(Date, 'now').mockImplementation(() => ++now)
    for (let i = 1; i <= 22; i += 1) {
      saveCachedEditedChapterKeys(`book_${i}`, [`https://example.org/${i}`])
    }

    const cache = readEditedChaptersCache()
    expect(Object.keys(cache)).toHaveLength(20)
    // 最新写入的还在；最早的两本被淘汰
    expect(loadCachedEditedChapterKeys('book_22')).toEqual(['https://example.org/22'])
    expect(loadCachedEditedChapterKeys('book_3')).toEqual(['https://example.org/3'])
    expect(loadCachedEditedChapterKeys('book_1')).toEqual([])
    expect(loadCachedEditedChapterKeys('book_2')).toEqual([])
    vi.restoreAllMocks()
  })
})
