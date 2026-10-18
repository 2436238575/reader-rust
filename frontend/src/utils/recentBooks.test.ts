// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'

const { getUserdata, saveUserdata } = vi.hoisted(() => ({
  getUserdata: vi.fn(),
  saveUserdata: vi.fn(() => Promise.resolve()),
}))

vi.mock('../api/userdata', () => ({ getUserdata, saveUserdata }))

import {
  clearRecentReadBooks,
  getRecentReadBookKey,
  loadRecentReadBooks,
  mergeRecentLists,
  saveRecentReadBook,
  syncRecentBooksFromBackend,
  type RecentReadBook,
} from './recentBooks'

function book(partial: Partial<RecentReadBook>): RecentReadBook {
  return {
    bookId: 'book-1',
    bookUrl: 'https://source.example/book/1',
    origin: 'https://source.example',
    name: '书名',
    author: '作者',
    ...partial,
  } as RecentReadBook
}

beforeEach(() => {
  localStorage.clear()
  vi.clearAllMocks()
})

describe('mergeRecentLists', () => {
  it('取并集，同一本书保留较新的记录并按时间降序', () => {
    const merged = mergeRecentLists(
      [book({ bookId: 'id-1', recentReadAt: 100 }), book({ bookId: 'id-2', recentReadAt: 300 })],
      [
        book({ bookId: 'id-1', recentReadAt: 200, name: '新版' }),
        book({ bookId: 'id-3', recentReadAt: 50 }),
      ]
    )

    expect(merged.map((item) => item.bookId)).toEqual(['id-2', 'id-1', 'id-3'])
    expect(merged.find((item) => item.bookId === 'id-1')?.name).toBe('新版')
  })

  it('过滤掉没有 bookId 的旧条目并截断到上限', () => {
    const many: RecentReadBook[] = Array.from({ length: 150 }, (_, i) =>
      book({ bookId: `id-${i}`, recentReadAt: i })
    )
    const withLegacy = [
      ...many,
      // 重构前按 origin::bookUrl 生成键、没有 bookId 的旧记录直接丢弃
      book({ bookId: '', recentReadAt: 999 }),
    ]

    const merged = mergeRecentLists(withLegacy, [])
    expect(merged).toHaveLength(100)
    expect(merged.some((item) => !item.bookId)).toBe(false)
    expect(merged.some((item) => item.bookId === 'id-149')).toBe(true)
    expect(merged.some((item) => item.bookId === 'id-0')).toBe(false)
  })
})

describe('本地读写与后端推送', () => {
  it('saveRecentReadBook 写本地并防抖推后端', async () => {
    vi.useFakeTimers()
    saveRecentReadBook(book({ bookId: 'id-1', recentReadAt: 100 }))

    expect(loadRecentReadBooks()).toHaveLength(1)
    expect(saveUserdata).not.toHaveBeenCalled()

    await vi.advanceTimersByTimeAsync(2500)
    expect(saveUserdata).toHaveBeenCalledTimes(1)
    expect(saveUserdata).toHaveBeenCalledWith('recent-books', [
      expect.objectContaining({ bookId: 'id-1' }),
    ])
    vi.useRealTimers()
  })

  it('连续保存合并为一次后端写入', async () => {
    vi.useFakeTimers()
    saveRecentReadBook(book({ bookId: 'id-1', recentReadAt: 100 }))
    saveRecentReadBook(book({ bookId: 'id-2', recentReadAt: 200 }))

    await vi.advanceTimersByTimeAsync(2500)
    expect(saveUserdata).toHaveBeenCalledTimes(1)
    const pushed = (saveUserdata.mock.calls[0] as unknown as [string, RecentReadBook[]])[1]
    expect(pushed.map((item) => item.bookId)).toEqual(['id-2', 'id-1'])
    vi.useRealTimers()
  })

  it('clearRecentReadBooks 清空本地并推空列表', async () => {
    vi.useFakeTimers()
    saveRecentReadBook(book({ bookId: 'id-1', recentReadAt: 100 }))
    clearRecentReadBooks()

    expect(loadRecentReadBooks()).toHaveLength(0)
    await vi.advanceTimersByTimeAsync(2500)
    expect(saveUserdata).toHaveBeenLastCalledWith('recent-books', [])
    vi.useRealTimers()
  })
})

describe('syncRecentBooksFromBackend', () => {
  it('与后端并集合并并写回两端', async () => {
    localStorage.setItem(
      'reader-recent-books',
      JSON.stringify([book({ bookId: 'id-1', recentReadAt: 100 })])
    )
    getUserdata.mockResolvedValue([book({ bookId: 'id-2', recentReadAt: 200 })])

    await syncRecentBooksFromBackend()

    const local = loadRecentReadBooks()
    expect(local.map((item) => item.bookId)).toEqual(['id-2', 'id-1'])
    // 合并结果与后端原文不一致 → 推回合并结果
    expect(saveUserdata).toHaveBeenCalledWith('recent-books', local)
  })

  it('后端已是最新时不重复推送', async () => {
    const remoteList = [book({ bookId: 'id-1', recentReadAt: 100 })]
    localStorage.setItem('reader-recent-books', JSON.stringify(remoteList))
    getUserdata.mockResolvedValue(remoteList)

    await syncRecentBooksFromBackend()

    expect(saveUserdata).not.toHaveBeenCalled()
  })

  it('后端不可达时保持本地不变', async () => {
    localStorage.setItem(
      'reader-recent-books',
      JSON.stringify([book({ bookId: 'id-1', recentReadAt: 100 })])
    )
    getUserdata.mockRejectedValue(new Error('offline'))

    await syncRecentBooksFromBackend()

    expect(loadRecentReadBooks().map((item) => item.bookId)).toEqual(['id-1'])
    expect(saveUserdata).not.toHaveBeenCalled()
  })

  it('同一分钟内重复调用被节流', async () => {
    // 假时钟越过前面用例留下的节流窗口，保证从干净状态开始
    vi.useFakeTimers()
    vi.setSystemTime(Date.now() + 61_000)
    getUserdata.mockResolvedValue([])
    await syncRecentBooksFromBackend()
    await syncRecentBooksFromBackend()
    expect(getUserdata).toHaveBeenCalledTimes(1)
    vi.useRealTimers()
  })
})

describe('getRecentReadBookKey', () => {
  it('以 bookId 为稳定键，缺省为空串', () => {
    expect(getRecentReadBookKey({ bookId: 'id-1' })).toBe('id-1')
    expect(getRecentReadBookKey({})).toBe('')
  })
})
