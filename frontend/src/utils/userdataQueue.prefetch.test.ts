// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const { httpGet, httpPost } = vi.hoisted(() => ({
  httpGet: vi.fn(),
  httpPost: vi.fn(),
}))

// 只 mock http 层：用真实的 api/userdata 预读缓存验证 keepalive 直发的写后失效口径
vi.mock('../api/http', () => ({
  default: { get: httpGet, post: httpPost },
}))

// 被测模块在 import 时挂 window 事件监听并持有模块级状态，每个用例都要全新模块
async function loadModules() {
  const api = await import('../api/userdata')
  const queue = await import('./userdataQueue')
  return { api, queue }
}

beforeEach(() => {
  vi.resetModules()
  vi.useFakeTimers()
  httpGet.mockReset()
  httpPost.mockReset().mockResolvedValue({ data: undefined })
  // 上一个用例 import 的队列实例在 window 上留有监听且可能有未发出的文档，先补发排空
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(undefined))
  window.dispatchEvent(new Event('pagehide'))
  vi.unstubAllGlobals()
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('keepalive 直发的预读缓存失效', () => {
  it('keepalive 成功后被直发文档的预读缓存失效，未发送的文档仍命中缓存', async () => {
    const fetchMock = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('fetch', fetchMock)
    const { api, queue } = await loadModules()
    httpGet.mockResolvedValueOnce({ data: { docs: { 'reading-stats': 1, 'recent-books': [] } } })
    await api.prefetchUserdata(['reading-stats', 'recent-books'])
    expect(httpGet).toHaveBeenCalledTimes(1)

    queue.queueUserdataSave('reading-stats', 2)
    window.dispatchEvent(new Event('pagehide'))
    expect(fetchMock).toHaveBeenCalledTimes(1)

    // 被直发的文档：预读缓存已失效，读取回落批量 GET
    httpGet.mockResolvedValueOnce({ data: { docs: { 'reading-stats': 2 } } })
    await expect(api.getUserdataBatch(['reading-stats'])).resolves.toEqual({ 'reading-stats': 2 })
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdataBatch?names=reading-stats')

    // 未被直发的文档：预读缓存仍命中，不发请求
    httpGet.mockClear()
    await expect(api.getUserdataBatch(['recent-books'])).resolves.toEqual({ 'recent-books': [] })
    expect(httpGet).not.toHaveBeenCalled()
  })
})
