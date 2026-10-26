// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const { httpGet, httpPost } = vi.hoisted(() => ({
  httpGet: vi.fn(),
  httpPost: vi.fn(),
}))

// http 实例的响应拦截器已把 ApiResponse 解包：这里直接模拟解包后的 { data }
vi.mock('./http', () => ({
  default: { get: httpGet, post: httpPost },
}))

import {
  clearUserdataPrefetch,
  getUserdataBatch,
  invalidateUserdataPrefetch,
  prefetchUserdata,
  saveUserdataBatch,
} from './userdata'

function mockBatchOnce(docs: Record<string, unknown>) {
  httpGet.mockResolvedValueOnce({ data: { docs } })
}

beforeEach(() => {
  vi.useFakeTimers()
  httpGet.mockReset()
  httpPost.mockReset().mockResolvedValue({ data: undefined })
  // 预读缓存是模块级状态，跨用例清空
  clearUserdataPrefetch()
})

afterEach(() => {
  vi.useRealTimers()
})

describe('prefetchUserdata 首屏批量预读', () => {
  it('一次预读只发一次批量请求', async () => {
    mockBatchOnce({ 'reading-stats': { totalSeconds: 1 } })

    await prefetchUserdata(['reading-stats', 'recent-books', 'readChapterHistory'])

    expect(httpGet).toHaveBeenCalledTimes(1)
    expect(httpGet).toHaveBeenCalledWith(
      '/getUserdataBatch?names=reading-stats,recent-books,readChapterHistory'
    )
  })

  it('names 为空时不发请求', async () => {
    await prefetchUserdata([])
    expect(httpGet).not.toHaveBeenCalled()
  })

  it('预读失败静默（不抛出），随后 getUserdataBatch 不受影响照常发批量 GET', async () => {
    httpGet.mockRejectedValueOnce(new Error('offline'))

    await expect(prefetchUserdata(['reading-stats'])).resolves.toBeUndefined()

    mockBatchOnce({ 'reading-stats': { totalSeconds: 5 } })
    const docs = await getUserdataBatch<{ totalSeconds: number }>(['reading-stats'])
    expect(httpGet).toHaveBeenCalledTimes(2)
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdataBatch?names=reading-stats')
    expect(docs['reading-stats']).toEqual({ totalSeconds: 5 })
  })
})

describe('getUserdataBatch 消费预读缓存', () => {
  it('命中缓存直接返回，不再发批量 GET', async () => {
    mockBatchOnce({
      'reading-stats': { totalSeconds: 7 },
      'recent-books': [{ bookId: 'b1' }],
      readChapterHistory: null,
    })
    await prefetchUserdata(['reading-stats', 'recent-books', 'readChapterHistory'])
    httpGet.mockClear()

    await expect(getUserdataBatch(['reading-stats'])).resolves.toEqual({
      'reading-stats': { totalSeconds: 7 },
    })
    await expect(getUserdataBatch(['recent-books'])).resolves.toEqual({
      'recent-books': [{ bookId: 'b1' }],
    })
    // 缺失文档也缓存为 null：命中直接返回 null，不补请求
    await expect(getUserdataBatch(['readChapterHistory'])).resolves.toEqual({
      readChapterHistory: null,
    })
    expect(httpGet).not.toHaveBeenCalled()
  })

  it('缓存一次性消费：第二次读取回落批量 GET', async () => {
    mockBatchOnce({ 'reading-stats': { totalSeconds: 7 } })
    await prefetchUserdata(['reading-stats'])
    await getUserdataBatch(['reading-stats'])

    mockBatchOnce({ 'reading-stats': { totalSeconds: 9 } })
    const docs = await getUserdataBatch<{ totalSeconds: number }>(['reading-stats'])
    expect(httpGet).toHaveBeenCalledTimes(2)
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdataBatch?names=reading-stats')
    expect(docs['reading-stats']).toEqual({ totalSeconds: 9 })
  })

  it('30s 内命中缓存，过期后回落批量 GET', async () => {
    mockBatchOnce({ 'reading-stats': 1 })
    await prefetchUserdata(['reading-stats'])

    vi.advanceTimersByTime(29_000)
    await expect(getUserdataBatch(['reading-stats'])).resolves.toEqual({ 'reading-stats': 1 })
    expect(httpGet).toHaveBeenCalledTimes(1)

    // 再次预读后放过期
    mockBatchOnce({ 'reading-stats': 2 })
    await prefetchUserdata(['reading-stats'])
    vi.advanceTimersByTime(31_000)
    mockBatchOnce({ 'reading-stats': 3 })
    await expect(getUserdataBatch(['reading-stats'])).resolves.toEqual({ 'reading-stats': 3 })
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdataBatch?names=reading-stats')
  })

  it('未预读的文档发批量 GET，name 做 URL 编码', async () => {
    mockBatchOnce({ 'a b': null })
    const docs = await getUserdataBatch(['a b'])
    expect(httpGet).toHaveBeenCalledWith('/getUserdataBatch?names=a%20b')
    expect(docs).toEqual({ 'a b': null })
  })

  it('部分命中时只对 miss 的条目发请求，结果与缓存命中合并', async () => {
    mockBatchOnce({ 'reading-stats': { totalSeconds: 7 } })
    await prefetchUserdata(['reading-stats'])

    mockBatchOnce({ 'recent-books': [] })
    const docs = await getUserdataBatch(['reading-stats', 'recent-books'])

    expect(httpGet).toHaveBeenCalledTimes(2) // 预读 1 次 + miss 1 次
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdataBatch?names=recent-books')
    expect(docs).toEqual({ 'reading-stats': { totalSeconds: 7 }, 'recent-books': [] })
  })
})

describe('写后缓存失效', () => {
  it('saveUserdataBatch 成功后对应缓存失效，下一次读取走批量 GET', async () => {
    mockBatchOnce({ 'reading-stats': 1, 'recent-books': [] })
    await prefetchUserdata(['reading-stats', 'recent-books'])

    await saveUserdataBatch([{ name: 'reading-stats', value: 2 }])

    // 被写的文档失效；未被写的文档仍命中缓存
    mockBatchOnce({ 'reading-stats': 3 })
    await expect(getUserdataBatch(['reading-stats'])).resolves.toEqual({ 'reading-stats': 3 })
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdataBatch?names=reading-stats')
    httpGet.mockClear()
    await expect(getUserdataBatch(['recent-books'])).resolves.toEqual({ 'recent-books': [] })
    expect(httpGet).not.toHaveBeenCalled()
  })

  it('写失败不清缓存：预读值仍可消费', async () => {
    mockBatchOnce({ 'reading-stats': 1 })
    await prefetchUserdata(['reading-stats'])

    httpPost.mockRejectedValueOnce(new Error('offline'))
    await expect(saveUserdataBatch([{ name: 'reading-stats', value: 2 }])).rejects.toThrow(
      'offline'
    )

    httpGet.mockClear()
    await expect(getUserdataBatch(['reading-stats'])).resolves.toEqual({ 'reading-stats': 1 })
    expect(httpGet).not.toHaveBeenCalled()
  })
})

describe('invalidateUserdataPrefetch 按文档名失效', () => {
  it('只失效指定文档，其他文档的预读缓存仍可消费', async () => {
    mockBatchOnce({ 'reading-stats': 1, 'recent-books': [] })
    await prefetchUserdata(['reading-stats', 'recent-books'])

    invalidateUserdataPrefetch(['reading-stats'])

    // 被失效的文档回落批量 GET；未失效的文档仍命中缓存，不发请求
    mockBatchOnce({ 'reading-stats': 2 })
    await expect(getUserdataBatch(['reading-stats'])).resolves.toEqual({ 'reading-stats': 2 })
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdataBatch?names=reading-stats')
    httpGet.mockClear()
    await expect(getUserdataBatch(['recent-books'])).resolves.toEqual({ 'recent-books': [] })
    expect(httpGet).not.toHaveBeenCalled()
  })
})

describe('clearUserdataPrefetch 登出清理', () => {
  it('清空后 getUserdataBatch 回落批量 GET', async () => {
    mockBatchOnce({ 'reading-stats': 1 })
    await prefetchUserdata(['reading-stats'])

    clearUserdataPrefetch()

    mockBatchOnce({ 'reading-stats': 2 })
    await expect(getUserdataBatch(['reading-stats'])).resolves.toEqual({ 'reading-stats': 2 })
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdataBatch?names=reading-stats')
  })
})
