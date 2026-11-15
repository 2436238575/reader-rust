import { beforeEach, describe, expect, it, vi } from 'vitest'
import { reportReadEvents } from '../api/readStats'
import {
  accumulateReadSeconds,
  currentReadDayKey,
  flushReadEvents,
  flushReadEventsKeepalive,
  pendingReadEvents,
  resetReadEventBuffer,
  syncReadDayKey,
  trackChapterRead,
} from './readEvents'

vi.mock('../api/readStats', () => ({
  reportReadEvents: vi.fn(),
}))

const reportMock = vi.mocked(reportReadEvents)

beforeEach(() => {
  resetReadEventBuffer()
  reportMock.mockReset()
  vi.unstubAllGlobals()
})

describe('readEvents', () => {
  it('按章累计当日秒数，flush 上报累计值且成功后保留正秒数', async () => {
    reportMock.mockResolvedValue({ accepted: 1, dayKey: 100 })
    trackChapterRead('book_1', 0)
    accumulateReadSeconds('book_1', 0, 10)

    await flushReadEvents()
    expect(reportMock).toHaveBeenLastCalledWith([{ bookId: 'book_1', chapterIndex: 0, seconds: 10 }])
    expect(currentReadDayKey()).toBe(100)
    // 累计语义：成功后保留 10，下次上报 10+5=15；未变化时不再重复发
    expect(pendingReadEvents()).toEqual([{ bookId: 'book_1', chapterIndex: 0, seconds: 10 }])
    reportMock.mockClear()
    await flushReadEvents()
    expect(reportMock).not.toHaveBeenCalled()

    accumulateReadSeconds('book_1', 0, 5)
    reportMock.mockResolvedValue({ accepted: 1, dayKey: 100 })
    await flushReadEvents()
    expect(reportMock).toHaveBeenLastCalledWith(
      [{ bookId: 'book_1', chapterIndex: 0, seconds: 15 }],
      100
    )
  })

  it('0 秒的「已读章」上报成功后丢掉，不会每次 flush 重发', async () => {
    reportMock.mockResolvedValue({ accepted: 1, dayKey: 100 })
    trackChapterRead('book_1', 3)
    await flushReadEvents()
    expect(reportMock).toHaveBeenLastCalledWith([{ bookId: 'book_1', chapterIndex: 3, seconds: 0 }])
    expect(pendingReadEvents()).toEqual([])
  })

  it('服务端 dayKey 变化时清空本地累计值', async () => {
    reportMock.mockResolvedValue({ accepted: 0, dayKey: 100 })
    await syncReadDayKey()
    trackChapterRead('book_1', 0)
    accumulateReadSeconds('book_1', 0, 42)

    reportMock.mockResolvedValue({ accepted: 1, dayKey: 101 })
    await flushReadEvents()
    expect(reportMock).toHaveBeenLastCalledWith(
      [{ bookId: 'book_1', chapterIndex: 0, seconds: 42 }],
      100
    )
    expect(currentReadDayKey()).toBe(101)
    expect(pendingReadEvents()).toEqual([])
  })

  it('超过 100 条自动分片', async () => {
    reportMock.mockResolvedValue({ accepted: 100, dayKey: 100 })
    for (let index = 0; index < 150; index += 1) {
      trackChapterRead('book_1', index)
    }
    await flushReadEvents()
    expect(reportMock).toHaveBeenCalledTimes(2)
    expect(reportMock.mock.calls[0][0]).toHaveLength(100)
    expect(reportMock.mock.calls[1][0]).toHaveLength(50)
  })

  it('pagehide keepalive 走裸 fetch 且带 CSRF 头', () => {
    const fetchMock = vi.fn(async () => new Response(null, { status: 200 }))
    vi.stubGlobal('fetch', fetchMock)
    trackChapterRead('book_1', 0)
    accumulateReadSeconds('book_1', 0, 3)

    flushReadEventsKeepalive()
    expect(fetchMock).toHaveBeenCalledTimes(1)
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit]
    expect(url).toContain('/v1/me/read-events:batch')
    expect(init.keepalive).toBe(true)
    expect(init.credentials).toBe('same-origin')
    expect((init.headers as Record<string, string>)['X-Requested-With']).toBe(
      'XMLHttpRequest'
    )
    expect(JSON.parse(String(init.body))).toEqual({
      dayKey: null,
      events: [{ bookId: 'book_1', chapterIndex: 0, seconds: 3 }],
    })
  })
})
