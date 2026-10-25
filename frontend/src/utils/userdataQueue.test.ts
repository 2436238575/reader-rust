// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const { saveUserdataBatch } = vi.hoisted(() => ({
  saveUserdataBatch: vi.fn(),
}))

vi.mock('../api/userdata', () => ({ saveUserdataBatch }))

// 被测模块在 import 时挂 window 事件监听并持有队列状态，每个用例都要全新模块
async function loadQueue() {
  return await import('./userdataQueue')
}

function deferred<T = undefined>() {
  let resolve!: (value: T | PromiseLike<T>) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

beforeEach(() => {
  vi.resetModules()
  vi.useFakeTimers()
  saveUserdataBatch.mockReset().mockResolvedValue(undefined)
  // 上一个用例 import 的模块实例在 window 上留有事件监听，其队列里可能还有
  // 未发送成功的文档；先补发排空，避免它们跟着本用例的事件一起触发
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(undefined))
  window.dispatchEvent(new Event('pagehide'))
  vi.unstubAllGlobals()
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('queueUserdataSave 合并与防抖', () => {
  it('同一文档多次入队只发最后一次', async () => {
    const queue = await loadQueue()
    queue.queueUserdataSave('reading-stats', { totalSeconds: 1 })
    queue.queueUserdataSave('reading-stats', { totalSeconds: 2 })
    queue.queueUserdataSave('reading-stats', { totalSeconds: 3 })

    await vi.advanceTimersByTimeAsync(200)

    expect(saveUserdataBatch).toHaveBeenCalledTimes(1)
    expect(saveUserdataBatch).toHaveBeenCalledWith([
      { name: 'reading-stats', value: { totalSeconds: 3 } },
    ])
  })

  it('不同文档在防抖窗口内合成一次批量写，成功后不重复发送', async () => {
    const queue = await loadQueue()
    queue.queueUserdataSave('reading-stats', { totalSeconds: 1 })
    queue.queueUserdataSave('recent-books', ['b1'])

    await vi.advanceTimersByTimeAsync(200)

    expect(saveUserdataBatch).toHaveBeenCalledTimes(1)
    expect(saveUserdataBatch).toHaveBeenCalledWith([
      { name: 'reading-stats', value: { totalSeconds: 1 } },
      { name: 'recent-books', value: ['b1'] },
    ])

    // 成功后队列已清空，后续没有重复发送
    await vi.advanceTimersByTimeAsync(10_000)
    expect(saveUserdataBatch).toHaveBeenCalledTimes(1)
  })

  it('防抖窗口外的写入进入下一批', async () => {
    const queue = await loadQueue()
    queue.queueUserdataSave('a', 1)
    await vi.advanceTimersByTimeAsync(200)
    queue.queueUserdataSave('a', 2)
    await vi.advanceTimersByTimeAsync(200)

    expect(saveUserdataBatch).toHaveBeenCalledTimes(2)
    expect(saveUserdataBatch).toHaveBeenNthCalledWith(1, [{ name: 'a', value: 1 }])
    expect(saveUserdataBatch).toHaveBeenNthCalledWith(2, [{ name: 'a', value: 2 }])
  })
})

describe('单飞', () => {
  it('在途批量未结束时新写入不并发，结束后只补一轮且带最新值', async () => {
    const first = deferred()
    saveUserdataBatch.mockReturnValueOnce(first.promise)
    const queue = await loadQueue()
    queue.queueUserdataSave('a', 1)
    await vi.advanceTimersByTimeAsync(200)
    expect(saveUserdataBatch).toHaveBeenCalledTimes(1)
    expect(saveUserdataBatch).toHaveBeenLastCalledWith([{ name: 'a', value: 1 }])

    // 在途期间：新写入 + 主动 flush 都不并发第二个请求
    queue.queueUserdataSave('a', 2)
    queue.queueUserdataSave('b', 9)
    const flushing = queue.flushUserdataQueue()
    await vi.advanceTimersByTimeAsync(5000)
    expect(saveUserdataBatch).toHaveBeenCalledTimes(1)

    // 在途结束后补发一轮：a=1 已发送，但已被新值覆盖，补发的是 a=2 与 b=9
    first.resolve(undefined)
    await flushing
    expect(saveUserdataBatch).toHaveBeenCalledTimes(2)
    expect(saveUserdataBatch).toHaveBeenLastCalledWith([
      { name: 'a', value: 2 },
      { name: 'b', value: 9 },
    ])
  })
})

describe('失败重试', () => {
  it('按退避间隔重试，至多 3 次后放弃', async () => {
    saveUserdataBatch.mockRejectedValue(new Error('offline'))
    const queue = await loadQueue()
    queue.queueUserdataSave('a', 1)

    await vi.advanceTimersByTimeAsync(200) // 首次发送，失败
    expect(saveUserdataBatch).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1000) // 第 1 次重试
    expect(saveUserdataBatch).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(2000) // 第 2 次重试
    expect(saveUserdataBatch).toHaveBeenCalledTimes(3)
    await vi.advanceTimersByTimeAsync(4000) // 第 3 次重试
    expect(saveUserdataBatch).toHaveBeenCalledTimes(4)

    // 重试预算已用完：不再自动重试，文档留在队列里等下一次写入带走
    await vi.advanceTimersByTimeAsync(120_000)
    expect(saveUserdataBatch).toHaveBeenCalledTimes(4)

    // 重试每次都带上未被覆盖的原文档
    for (const call of saveUserdataBatch.mock.calls) {
      expect(call).toEqual([[{ name: 'a', value: 1 }]])
    }
  })

  it('flushUserdataQueue 失败会 reject，调用方可自行兜底', async () => {
    saveUserdataBatch.mockRejectedValueOnce(new Error('offline'))
    const queue = await loadQueue()
    queue.queueUserdataSave('a', 1)
    await expect(queue.flushUserdataQueue()).rejects.toThrow('offline')
  })
})

describe('页面卸载补发', () => {
  it('pagehide 时用裸 fetch + keepalive 补发当前队列', async () => {
    const fetchMock = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('fetch', fetchMock)
    const queue = await loadQueue()
    queue.queueUserdataSave('reading-stats', { totalSeconds: 7 })
    queue.queueUserdataSave('recent-books', [])

    window.dispatchEvent(new Event('pagehide'))

    expect(fetchMock).toHaveBeenCalledTimes(1)
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit]
    // URL 经 API_BASE 拼接，不写死路径；凭据走同源 Cookie
    expect(url).toBe('/reader3/saveUserdataBatch')
    expect(init.method).toBe('POST')
    expect(init.keepalive).toBe(true)
    expect(init.credentials).toBe('same-origin')
    expect(JSON.parse(init.body as string)).toEqual({
      docs: [
        { name: 'reading-stats', value: { totalSeconds: 7 } },
        { name: 'recent-books', value: [] },
      ],
    })

    // 补发后队列已清，防抖不再经 axios 重复发送
    await vi.advanceTimersByTimeAsync(10_000)
    expect(saveUserdataBatch).not.toHaveBeenCalled()
  })

  it('visibilitychange 进入 hidden 时同样补发', async () => {
    const fetchMock = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('fetch', fetchMock)
    const queue = await loadQueue()
    queue.queueUserdataSave('a', 1)

    const descriptor = Object.getOwnPropertyDescriptor(document, 'visibilityState')
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' })
    try {
      document.dispatchEvent(new Event('visibilitychange'))
    } finally {
      if (descriptor) Object.defineProperty(document, 'visibilityState', descriptor)
    }

    expect(fetchMock).toHaveBeenCalledTimes(1)
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit]
    expect(url).toBe('/reader3/saveUserdataBatch')
    expect(init.keepalive).toBe(true)
  })
})
