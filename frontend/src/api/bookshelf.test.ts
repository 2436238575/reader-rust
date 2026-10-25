// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import { deleteBookCache, getChapterList, getCoverUrl, setBookSource } from './bookshelf'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
  },
}))

const postMock = http.post as unknown as Mock

describe('getCoverUrl', () => {
  beforeEach(() => {
    localStorage.clear()
  })

  it('passes the backend image route through with the token', () => {
    localStorage.setItem('accessToken', 'tok')
    // 后端把书源地址改写成了本站取图地址：id 由后端映射表维护，前端原样使用
    const url = getCoverUrl('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3')
    expect(url).toBe('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3?accessToken=tok')
  })

  it('still routes raw source urls through the compat cover endpoint', () => {
    localStorage.setItem('accessToken', 'tok')
    // 历史书架记录里存的是书源地址（含签名），走兼容入口
    const url = getCoverUrl('https://img.example/a.heic?x-signature=abc')
    expect(url.startsWith('/reader3/cover?path=')).toBe(true)
    expect(url).toContain('accessToken=tok')
    expect(url).toContain(encodeURIComponent('https://img.example/a.heic?x-signature=abc'))
  })

  it('returns empty for missing cover', () => {
    expect(getCoverUrl(undefined)).toBe('')
    expect(getCoverUrl('')).toBe('')
  })
})

describe('getChapterList 会话内缓存', () => {
  beforeEach(() => {
    postMock.mockReset()
    postMock.mockResolvedValue({ data: [{ title: '第一章' }] })
  })

  it('TTL 内两次调用只请求一次', async () => {
    const first = await getChapterList({ bookId: 'ttl-book' })
    const second = await getChapterList({ bookId: 'ttl-book' })

    expect(postMock).toHaveBeenCalledTimes(1)
    expect(postMock).toHaveBeenCalledWith('/getChapterList', { bookId: 'ttl-book' })
    expect(second).toEqual(first)
  })

  it('并发的同书请求合并为一次', async () => {
    let release!: (value: { data: unknown }) => void
    postMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          release = resolve
        })
    )
    const first = getChapterList({ bookId: 'concurrent-book' })
    const second = getChapterList({ bookId: 'concurrent-book' })
    release({ data: [{ title: '第一章' }] })
    const [a, b] = await Promise.all([first, second])

    expect(postMock).toHaveBeenCalledTimes(1)
    expect(b).toEqual(a)
  })

  it('refresh=1 绕过缓存重新请求并更新缓存', async () => {
    postMock.mockResolvedValueOnce({ data: [{ title: '第一版' }] })
    postMock.mockResolvedValueOnce({ data: [{ title: '第二版' }] })

    const initial = await getChapterList({ bookId: 'refresh-book', refresh: 1 })
    expect(postMock).toHaveBeenCalledTimes(1)

    // refresh 的结果已写回缓存：不带 refresh 的后续调用不再请求
    const cached = await getChapterList({ bookId: 'refresh-book' })
    expect(cached).toEqual(initial)
    expect(postMock).toHaveBeenCalledTimes(1)

    // 再次 refresh=1 绕过旧缓存，拿到新结果并覆盖缓存
    const refreshed = await getChapterList({ bookId: 'refresh-book', refresh: 1 })
    expect(postMock).toHaveBeenCalledTimes(2)
    expect(refreshed).toEqual([{ title: '第二版' }])
    const after = await getChapterList({ bookId: 'refresh-book' })
    expect(after).toEqual([{ title: '第二版' }])
    expect(postMock).toHaveBeenCalledTimes(2)
  })

  it('请求失败不缓存失败结果', async () => {
    postMock.mockRejectedValueOnce(new Error('网络错误'))
    await expect(getChapterList({ bookId: 'fail-book' })).rejects.toThrow('网络错误')

    const retry = await getChapterList({ bookId: 'fail-book' })
    expect(postMock).toHaveBeenCalledTimes(2)
    expect(retry).toEqual([{ title: '第一章' }])
  })

  it('不同 bookId 互不影响', async () => {
    await getChapterList({ bookId: 'book-a' })
    await getChapterList({ bookId: 'book-b' })
    expect(postMock).toHaveBeenCalledTimes(2)

    await getChapterList({ bookId: 'book-a' })
    await getChapterList({ bookId: 'book-b' })
    expect(postMock).toHaveBeenCalledTimes(2)
  })

  it('setBookSource 成功后失效该书的目录缓存', async () => {
    await getChapterList({ bookId: 'switch-book' })
    await setBookSource({ bookId: 'switch-book', targetBookId: 'candidate-1' })
    expect(postMock).toHaveBeenCalledTimes(2)

    // 换源后目录已整体变化：缓存失效，重新请求
    await getChapterList({ bookId: 'switch-book' })
    expect(postMock).toHaveBeenCalledTimes(3)
  })

  it('deleteBookCache 成功后失效该书的目录缓存', async () => {
    await getChapterList({ bookId: 'purge-book' })
    await deleteBookCache('purge-book')
    expect(postMock).toHaveBeenCalledTimes(2)

    // 服务端已按书清掉目录缓存：本地缓存同步失效
    await getChapterList({ bookId: 'purge-book' })
    expect(postMock).toHaveBeenCalledTimes(3)
  })

  it('换源时丢弃在途的旧目录请求，旧响应不会写回缓存', async () => {
    let releaseOld!: (value: { data: unknown }) => void
    postMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          releaseOld = resolve
        })
    )
    const oldRequest = getChapterList({ bookId: 'switch-inflight' })
    await setBookSource({ bookId: 'switch-inflight', targetBookId: 'candidate-2' })

    postMock.mockResolvedValueOnce({ data: [{ title: '新源目录' }] })
    const fresh = await getChapterList({ bookId: 'switch-inflight' })
    expect(fresh).toEqual([{ title: '新源目录' }])

    // 旧请求迟到：调用方仍拿到它自己的结果，但不能覆盖新源缓存
    releaseOld({ data: [{ title: '旧源目录' }] })
    expect(await oldRequest).toEqual([{ title: '旧源目录' }])

    const cached = await getChapterList({ bookId: 'switch-inflight' })
    expect(cached).toEqual([{ title: '新源目录' }])
    expect(postMock).toHaveBeenCalledTimes(3)
  })

  it('缓存最多保留 20 本，按插入序淘汰最旧', async () => {
    // 前面的测试可能已占用部分名额：一次性插入 25 本新书，
    // 无论此前状态如何，缓存最终恰好保留最后 20 本（bulk-5..bulk-24）
    for (let i = 0; i < 25; i++) {
      postMock.mockResolvedValueOnce({ data: [{ title: `第${i}章` }] })
      await getChapterList({ bookId: `bulk-${i}` })
    }
    const postsAfterFill = postMock.mock.calls.length

    // bulk-4 及更早的都已被淘汰：再次请求会重新发请求
    await getChapterList({ bookId: 'bulk-4' })
    expect(postMock.mock.calls.length).toBe(postsAfterFill + 1)

    // 最后写入的 bulk-24 仍在缓存里
    await getChapterList({ bookId: 'bulk-24' })
    expect(postMock.mock.calls.length).toBe(postsAfterFill + 1)
  })
})
