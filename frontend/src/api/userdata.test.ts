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
  getUserdata,
  prefetchUserdata,
  saveUserdata,
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

  it('预读失败静默（不抛出），随后 getUserdata 不受影响走单文档 GET', async () => {
    httpGet.mockRejectedValueOnce(new Error('offline'))

    await expect(prefetchUserdata(['reading-stats'])).resolves.toBeUndefined()

    httpGet.mockResolvedValueOnce({ data: { totalSeconds: 5 } })
    const value = await getUserdata<{ totalSeconds: number }>('reading-stats')
    expect(httpGet).toHaveBeenCalledTimes(2)
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdata?name=reading-stats')
    expect(value).toEqual({ totalSeconds: 5 })
  })
})

describe('getUserdata 消费预读缓存', () => {
  it('命中缓存直接返回，不再发单文档 GET', async () => {
    mockBatchOnce({
      'reading-stats': { totalSeconds: 7 },
      'recent-books': [{ bookId: 'b1' }],
      readChapterHistory: null,
    })
    await prefetchUserdata(['reading-stats', 'recent-books', 'readChapterHistory'])
    httpGet.mockClear()

    await expect(getUserdata('reading-stats')).resolves.toEqual({ totalSeconds: 7 })
    await expect(getUserdata('recent-books')).resolves.toEqual([{ bookId: 'b1' }])
    // 缺失文档也缓存为 null：命中直接返回 null，不补单文档 GET
    await expect(getUserdata('readChapterHistory')).resolves.toBeNull()
    expect(httpGet).not.toHaveBeenCalled()
  })

  it('缓存一次性消费：第二次读取回落单文档 GET', async () => {
    mockBatchOnce({ 'reading-stats': { totalSeconds: 7 } })
    await prefetchUserdata(['reading-stats'])
    await getUserdata('reading-stats')

    httpGet.mockResolvedValueOnce({ data: { totalSeconds: 9 } })
    const value = await getUserdata('reading-stats')
    expect(httpGet).toHaveBeenCalledTimes(2) // 批量 1 次 + 单文档 1 次
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdata?name=reading-stats')
    expect(value).toEqual({ totalSeconds: 9 })
  })

  it('30s 内命中缓存，过期后回落单文档 GET', async () => {
    mockBatchOnce({ 'reading-stats': 1 })
    await prefetchUserdata(['reading-stats'])

    vi.advanceTimersByTime(29_000)
    await expect(getUserdata('reading-stats')).resolves.toBe(1)
    expect(httpGet).toHaveBeenCalledTimes(1)

    // 再次预读后放过期
    mockBatchOnce({ 'reading-stats': 2 })
    await prefetchUserdata(['reading-stats'])
    vi.advanceTimersByTime(31_000)
    httpGet.mockResolvedValueOnce({ data: 3 })
    await expect(getUserdata('reading-stats')).resolves.toBe(3)
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdata?name=reading-stats')
  })

  it('未预读的文档走单文档 GET，name 做 URL 编码', async () => {
    httpGet.mockResolvedValueOnce({ data: null })
    const value = await getUserdata('a b')
    expect(httpGet).toHaveBeenCalledWith('/getUserdata?name=a%20b')
    expect(value).toBeNull()
  })
})

describe('写后缓存失效', () => {
  it('saveUserdata 成功后对应缓存失效，下一次读取走单文档 GET', async () => {
    mockBatchOnce({ 'reading-stats': 1 })
    await prefetchUserdata(['reading-stats'])

    await saveUserdata('reading-stats', 2)

    httpGet.mockResolvedValueOnce({ data: 3 })
    await expect(getUserdata('reading-stats')).resolves.toBe(3)
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdata?name=reading-stats')
  })

  it('saveUserdataBatch 成功后对应缓存失效', async () => {
    mockBatchOnce({ 'reading-stats': 1, 'recent-books': [] })
    await prefetchUserdata(['reading-stats', 'recent-books'])

    await saveUserdataBatch([{ name: 'reading-stats', value: 2 }])

    // 被写的文档失效；未被写的文档仍命中缓存
    httpGet.mockResolvedValueOnce({ data: 3 })
    await expect(getUserdata('reading-stats')).resolves.toBe(3)
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdata?name=reading-stats')
    httpGet.mockClear()
    await expect(getUserdata('recent-books')).resolves.toEqual([])
    expect(httpGet).not.toHaveBeenCalled()
  })

  it('写失败不清缓存：预读值仍可消费', async () => {
    mockBatchOnce({ 'reading-stats': 1 })
    await prefetchUserdata(['reading-stats'])

    httpPost.mockRejectedValueOnce(new Error('offline'))
    await expect(saveUserdata('reading-stats', 2)).rejects.toThrow('offline')

    httpGet.mockClear()
    await expect(getUserdata('reading-stats')).resolves.toBe(1)
    expect(httpGet).not.toHaveBeenCalled()
  })
})

describe('clearUserdataPrefetch 登出清理', () => {
  it('清空后 getUserdata 回落单文档 GET', async () => {
    mockBatchOnce({ 'reading-stats': 1 })
    await prefetchUserdata(['reading-stats'])

    clearUserdataPrefetch()

    httpGet.mockResolvedValueOnce({ data: 2 })
    await expect(getUserdata('reading-stats')).resolves.toBe(2)
    expect(httpGet).toHaveBeenLastCalledWith('/getUserdata?name=reading-stats')
  })
})
