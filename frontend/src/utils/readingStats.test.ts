// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'

const { getUserdataBatch, saveUserdataBatch, queueUserdataSave } = vi.hoisted(() => ({
  getUserdataBatch: vi.fn(),
  saveUserdataBatch: vi.fn(() => Promise.resolve()),
  queueUserdataSave: vi.fn(),
}))

vi.mock('../api/userdata', () => ({ getUserdataBatch, saveUserdataBatch }))
vi.mock('./userdataQueue', () => ({ queueUserdataSave }))

import {
  loadLocalReadingStats,
  mergeReadingStats,
  pushReadingStatsToBackend,
  sanitizeReadingStats,
  syncReadingStatsFromBackend,
  type ReadingStats,
} from './readingStats'

function stats(partial: Partial<ReadingStats>): ReadingStats {
  return {
    totalSeconds: 0,
    openedBooks: [],
    readChapters: [],
    completedBooks: [],
    ...partial,
  }
}

beforeEach(() => {
  localStorage.clear()
  vi.clearAllMocks()
})

describe('sanitizeReadingStats', () => {
  it('形状不对时回退默认值', () => {
    expect(sanitizeReadingStats(null)).toEqual(stats({}))
    expect(sanitizeReadingStats('x')).toEqual(stats({}))
    expect(sanitizeReadingStats({ totalSeconds: 'abc', openedBooks: 'no' })).toEqual(stats({}))
  })

  it('容错解析并对数组去重', () => {
    const cleaned = sanitizeReadingStats({
      totalSeconds: 12.9,
      openedBooks: ['a', 'a', 'b'],
      readChapters: 'bad',
    })
    expect(cleaned.totalSeconds).toBe(12)
    expect(cleaned.openedBooks).toEqual(['a', 'b'])
    expect(cleaned.readChapters).toEqual([])
  })
})

describe('mergeReadingStats', () => {
  it('时长取较大值而不是求和', () => {
    const merged = mergeReadingStats(stats({ totalSeconds: 3600 }), stats({ totalSeconds: 600 }))
    expect(merged.totalSeconds).toBe(3600)
  })

  it('集合取并集', () => {
    const merged = mergeReadingStats(
      stats({ openedBooks: ['a', 'b'], readChapters: ['a#1'], completedBooks: ['a'] }),
      stats({ openedBooks: ['b', 'c'], readChapters: ['a#2'], completedBooks: [] })
    )
    expect(merged.openedBooks).toEqual(['a', 'b', 'c'])
    expect(merged.readChapters).toEqual(['a#1', 'a#2'])
    expect(merged.completedBooks).toEqual(['a'])
  })

  it('readChapters 超限时裁掉最旧的', () => {
    const big = Array.from({ length: 5010 }, (_, i) => `b#${i}`)
    const merged = mergeReadingStats(stats({ readChapters: big }), stats({}))
    expect(merged.readChapters).toHaveLength(5000)
    expect(merged.readChapters[0]).toBe('b#10')
  })
})

describe('syncReadingStatsFromBackend', () => {
  it('与后端合并并写回本地与后端', async () => {
    const local = stats({ totalSeconds: 600, openedBooks: ['a'] })
    localStorage.setItem('reader-stats', JSON.stringify(local))
    getUserdataBatch.mockResolvedValue({
      'reading-stats': { totalSeconds: 1200, openedBooks: ['b'] },
    })

    const merged = await syncReadingStatsFromBackend(local)

    expect(merged.totalSeconds).toBe(1200)
    expect(merged.openedBooks).toEqual(['a', 'b'])
    expect(JSON.parse(localStorage.getItem('reader-stats')!)).toEqual(merged)
    expect(saveUserdataBatch).toHaveBeenCalledWith([{ name: 'reading-stats', value: merged }])
  })

  it('后端不可达时保持本地', async () => {
    const local = stats({ totalSeconds: 600 })
    getUserdataBatch.mockRejectedValue(new Error('offline'))

    const merged = await syncReadingStatsFromBackend(local)
    expect(merged).toEqual(local)
    expect(saveUserdataBatch).not.toHaveBeenCalled()
  })
})

describe('pushReadingStatsToBackend', () => {
  it('推入 userdata 批量写队列而不直接落库', () => {
    const value = stats({ totalSeconds: 60 })
    pushReadingStatsToBackend(value)
    expect(queueUserdataSave).toHaveBeenCalledWith('reading-stats', value)
    expect(saveUserdataBatch).not.toHaveBeenCalled()
  })
})

describe('loadLocalReadingStats', () => {
  it('坏数据回退空统计', () => {
    localStorage.setItem('reader-stats', '{broken')
    expect(loadLocalReadingStats()).toEqual(stats({}))
  })
})
