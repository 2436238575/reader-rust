// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import {
  deleteBookCache,
  getBookshelfWithCacheInfo,
  getChapterList,
  getCoverUrl,
  getShelfBook,
  setBookSource,
  withImageWidth,
} from './bookshelf'

import { API_V1_BASE } from '../utils/appBase'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
  },
  UPLOAD_TIMEOUT_MS: 900_000,
}))

const getMock = http.get as unknown as Mock
const postMock = http.post as unknown as Mock

describe('getCoverUrl', () => {
  it('后端取图地址直接可用，不再携带令牌（会话 Cookie 随 <img> 自动携带）', () => {
    // 后端把书源地址改写成了本站取图地址：id 由后端映射表维护，前端原样使用
    const url = getCoverUrl('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3')
    expect(url).toBe('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3')
  })

  it('本地书资源地址保留查询串且不带令牌', () => {
    const url = getCoverUrl('/reader3/localEpubAsset?path=user_1/a.epub')
    expect(url).toBe('/reader3/localEpubAsset?path=user_1/a.epub')
  })

  it('仍把书源原始地址走兼容封面入口，且不带令牌', () => {
    // 历史书架记录里存的是书源地址（含签名），走兼容入口
    const url = getCoverUrl('https://img.example/a.heic?x-signature=abc')
    expect(url.startsWith('/reader3/cover?path=')).toBe(true)
    expect(url).not.toContain('accessToken')
    expect(url).toContain(encodeURIComponent('https://img.example/a.heic?x-signature=abc'))
  })

  it('returns empty for missing cover', () => {
    expect(getCoverUrl(undefined)).toBe('')
    expect(getCoverUrl('')).toBe('')
  })

  it('本站取图地址带宽度：?w=<width>', () => {
    const url = getCoverUrl('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3', 320)
    expect(url).toBe('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3?w=320')
  })

  it('本站取图地址已有查询串时用 & 拼宽度', () => {
    const url = getCoverUrl('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3?x=1', 320)
    expect(url).toBe('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3?x=1&w=320')
  })

  it('兼容封面入口（书源原始地址）同样带宽度', () => {
    const url = getCoverUrl('https://img.example/a.jpg', 640)
    expect(url.startsWith('/reader3/cover?path=')).toBe(true)
    expect(url).toContain('&w=640')
    expect(url).toContain(encodeURIComponent('https://img.example/a.jpg'))
  })

  it('本地 epub 资源与非本站地址不加宽度', () => {
    expect(getCoverUrl('/reader3/localEpubAsset?path=user_1/a.epub', 320)).toBe(
      '/reader3/localEpubAsset?path=user_1/a.epub'
    )
    // 不以 http 或 / 开头的地址（如 data: URI）原样返回，不参与图片管道
    const dataUri = 'data:image/png;base64,iVBORw0KGgo='
    expect(getCoverUrl(dataUri, 320)).toBe(dataUri)
  })

  it('不传宽度时地址保持原样', () => {
    expect(getCoverUrl('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3')).toBe(
      '/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3'
    )
    expect(getCoverUrl('https://img.example/a.jpg')).not.toContain('w=')
  })
})

describe('withImageWidth', () => {
  it('本站取图地址追加 ?w=<width>', () => {
    expect(withImageWidth('/reader3/image/abc123', 1200)).toBe('/reader3/image/abc123?w=1200')
  })

  it('已有查询串时用 & 拼接', () => {
    expect(withImageWidth('/reader3/image/abc123?x=1', 480)).toBe('/reader3/image/abc123?x=1&w=480')
    expect(withImageWidth('/reader3/cover?path=https%3A%2F%2Fa.example%2Fx.jpg', 320)).toBe(
      '/reader3/cover?path=https%3A%2F%2Fa.example%2Fx.jpg&w=320'
    )
  })

  it('第三方直链、本地资源与其他地址一律原样返回', () => {
    expect(withImageWidth('https://cdn.example/a.jpg', 480)).toBe('https://cdn.example/a.jpg')
    expect(withImageWidth('http://cdn.example/a.jpg?y=2', 480)).toBe('http://cdn.example/a.jpg?y=2')
    expect(withImageWidth('/reader3/localEpubAsset?path=user_1/a.epub', 480)).toBe(
      '/reader3/localEpubAsset?path=user_1/a.epub'
    )
    expect(withImageWidth('/reader3/getBookContent', 480)).toBe('/reader3/getBookContent')
  })

  it('宽度缺失或非正数时不加参数', () => {
    expect(withImageWidth('/reader3/image/abc123')).toBe('/reader3/image/abc123')
    expect(withImageWidth('/reader3/image/abc123', 0)).toBe('/reader3/image/abc123')
  })
})

describe('书架读取（v1 ShelfItem）', () => {
  const shelfItem = (overrides: Record<string, unknown> = {}) => ({
    bookId: 'book_1',
    sourceId: 'src_7',
    name: '书',
    author: '作者',
    bookUrl: 'https://up.example/book',
    origin: 'https://source.example',
    originName: '源',
    coverImageId: 'abc123',
    customCoverUrl: null,
    intro: '简介',
    kind: '玄幻',
    charset: 'utf-8',
    canUpdate: true,
    group: 4,
    progress: { chapterIndex: 3, chapterTitle: '第四章', offset: 0.25 },
    lastReadAt: '2026-11-08T11:20:00Z',
    latestChapterTitle: '最新章',
    totalChapters: 100,
    wordCount: '12万字',
    lastCheckAt: '2026-11-08T10:00:00Z',
    cachedChapters: 9,
    position: 2,
    ...overrides,
  })

  beforeEach(() => {
    getMock.mockReset()
  })

  it('getShelfBook 把 v1 条目映射回旧 Book 形状', async () => {
    getMock.mockResolvedValue({ data: shelfItem() })

    const book = await getShelfBook('book_1')

    expect(getMock).toHaveBeenCalledWith('/me/shelf/book_1', { baseURL: API_V1_BASE })
    expect(book).toMatchObject({
      bookId: 'book_1',
      bookSourceId: 7,
      coverUrl: '/reader3/image/abc123',
      group: 4,
      durChapterIndex: 3,
      durChapterPos: 2500,
      durChapterTitle: '第四章',
      durChapterTime: Math.floor(Date.parse('2026-11-08T11:20:00Z') / 1000),
      cachedChapterCount: 9,
      totalChapterNum: 100,
      latestChapterTitle: '最新章',
    })
  })

  it('getBookshelfWithCacheInfo 按游标拉全量并映射', async () => {
    getMock
      .mockResolvedValueOnce({
        data: {
          items: [shelfItem({ bookId: 'book_1', position: 0 })],
          nextCursor: 'c1',
          total: 2,
        },
      })
      .mockResolvedValueOnce({
        data: {
          items: [shelfItem({ bookId: 'book_2', position: 1 })],
          nextCursor: null,
          total: 2,
        },
      })

    const books = await getBookshelfWithCacheInfo()

    expect(books.map((book) => book.bookId)).toEqual(['book_1', 'book_2'])
    expect(getMock).toHaveBeenCalledTimes(2)
    expect(getMock.mock.calls[0][1]).toMatchObject({ params: { limit: 200 } })
    expect(getMock.mock.calls[1][1]).toMatchObject({
      params: { limit: 200, cursor: 'c1' },
    })
  })
})

describe('getChapterList 会话内缓存（v1 游标分页）', () => {
  /** v1 分页响应形状 */
  const page = (chapters: { title: string; url?: string }[]) => ({
    data: {
      items: chapters.map((chapter, index) => ({
        index,
        title: chapter.title,
        url: chapter.url ?? `https://up.example/ch/${index}`,
        isVip: false,
        isPay: false,
        isVolume: false,
        volume: null,
        wordCount: null,
        titleOverridden: false,
      })),
      nextCursor: null,
      stale: false,
      complete: true,
      tocVersion: null,
    },
  })
  const v1Get = (extra: Record<string, unknown> = {}) => ({
    baseURL: API_V1_BASE,
    params: { limit: 200, ...extra },
    timeout: 20_000,
  })

  beforeEach(() => {
    getMock.mockReset()
    getMock.mockResolvedValue(page([{ title: '第一章' }]))
    postMock.mockReset()
    postMock.mockResolvedValue({ data: '' })
  })

  it('GET v1 目录：浏览器按 ETag 自动协商 304，不用 POST', async () => {
    await getChapterList({ bookId: 'get-book', refresh: 1 })

    expect(getMock).toHaveBeenCalledTimes(1)
    expect(getMock).toHaveBeenCalledWith('/books/get-book/chapters', v1Get({ refresh: true }))
    expect(postMock).not.toHaveBeenCalled()
  })

  it('TTL 内两次调用只请求一次', async () => {
    const first = await getChapterList({ bookId: 'ttl-book' })
    const second = await getChapterList({ bookId: 'ttl-book' })

    expect(getMock).toHaveBeenCalledTimes(1)
    expect(getMock).toHaveBeenCalledWith('/books/ttl-book/chapters', v1Get())
    expect(second).toEqual(first)
  })

  it('并发的同书请求合并为一次', async () => {
    let release!: (value: unknown) => void
    getMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          release = resolve
        })
    )
    const first = getChapterList({ bookId: 'concurrent-book' })
    const second = getChapterList({ bookId: 'concurrent-book' })
    release(page([{ title: '第一章' }]))
    const [a, b] = await Promise.all([first, second])

    expect(getMock).toHaveBeenCalledTimes(1)
    expect(b).toEqual(a)
  })

  it('refresh=1 绕过缓存重新请求并更新缓存', async () => {
    getMock.mockResolvedValueOnce(page([{ title: '第一版' }]))
    getMock.mockResolvedValueOnce(page([{ title: '第二版' }]))

    const initial = await getChapterList({ bookId: 'refresh-book', refresh: 1 })
    expect(initial[0].title).toBe('第一版')
    expect(getMock).toHaveBeenCalledTimes(1)

    const cached = await getChapterList({ bookId: 'refresh-book' })
    expect(cached).toEqual(initial)
    expect(getMock).toHaveBeenCalledTimes(1)

    const refreshed = await getChapterList({ bookId: 'refresh-book', refresh: 1 })
    expect(getMock).toHaveBeenCalledTimes(2)
    expect(refreshed[0].title).toBe('第二版')
    const after = await getChapterList({ bookId: 'refresh-book' })
    expect(after[0].title).toBe('第二版')
    expect(getMock).toHaveBeenCalledTimes(2)
  })

  it('stale 目录使用短 TTL：过期后的下一次读取会重新请求', async () => {
    vi.useFakeTimers()
    try {
      getMock.mockResolvedValue({
        data: { ...page([{ title: '旧目录' }]).data, stale: true },
      })
      await getChapterList({ bookId: 'stale-book' })
      await getChapterList({ bookId: 'stale-book' })
      expect(getMock).toHaveBeenCalledTimes(1)

      vi.advanceTimersByTime(11_000)
      await getChapterList({ bookId: 'stale-book' })
      expect(getMock).toHaveBeenCalledTimes(2)
    } finally {
      vi.useRealTimers()
    }
  })

  it('未抓全的目录同样使用短 TTL', async () => {
    vi.useFakeTimers()
    try {
      getMock.mockResolvedValue({
        data: { ...page([{ title: '部分目录' }]).data, complete: false },
      })
      await getChapterList({ bookId: 'incomplete-book' })
      await getChapterList({ bookId: 'incomplete-book' })
      expect(getMock).toHaveBeenCalledTimes(1)

      vi.advanceTimersByTime(11_000)
      await getChapterList({ bookId: 'incomplete-book' })
      expect(getMock).toHaveBeenCalledTimes(2)
    } finally {
      vi.useRealTimers()
    }
  })

  it('续页收到 STALE_CURSOR 时从第一页重拉一次', async () => {
    getMock.mockResolvedValueOnce({
      data: {
        ...page([{ title: '第一章' }, { title: '第二章' }]).data,
        nextCursor: 'cursor-1',
        tocVersion: 7,
      },
    })
    getMock.mockRejectedValueOnce(
      Object.assign(new Error('目录已刷新'), { code: 'STALE_CURSOR' })
    )
    getMock.mockResolvedValueOnce({
      data: { ...page([{ title: '新第一章' }]).data, tocVersion: 8 },
    })

    const chapters = await getChapterList({ bookId: 'stale-cursor-book' })

    expect(chapters.map((chapter) => chapter.title)).toEqual(['新第一章'])
    expect(getMock).toHaveBeenCalledTimes(3)
    // 第二页带上了第一页的 tocVersion；重拉从无游标的第一页开始
    expect(getMock.mock.calls[1][1].params).toMatchObject({
      cursor: 'cursor-1',
      tocVersion: 7,
    })
    expect(getMock.mock.calls[2][1].params).toEqual({ limit: 200 })
  })

  it('请求失败不缓存失败结果', async () => {
    getMock.mockRejectedValueOnce(new Error('网络错误'))
    await expect(getChapterList({ bookId: 'fail-book' })).rejects.toThrow('网络错误')

    const retry = await getChapterList({ bookId: 'fail-book' })
    expect(getMock).toHaveBeenCalledTimes(2)
    expect(retry[0].title).toBe('第一章')
  })

  it('不同 bookId 互不影响', async () => {
    await getChapterList({ bookId: 'book-a' })
    await getChapterList({ bookId: 'book-b' })
    expect(getMock).toHaveBeenCalledTimes(2)

    await getChapterList({ bookId: 'book-a' })
    await getChapterList({ bookId: 'book-b' })
    expect(getMock).toHaveBeenCalledTimes(2)
  })

  it('deleteBookCache 成功后失效该书的目录缓存', async () => {
    await getChapterList({ bookId: 'purge-book' })
    expect(getMock).toHaveBeenCalledTimes(1)
    await deleteBookCache('purge-book')
    expect(postMock).toHaveBeenCalledWith('/deleteBookCache', { bookId: 'purge-book' })

    await getChapterList({ bookId: 'purge-book' })
    expect(getMock).toHaveBeenCalledTimes(2)
  })

  it('setBookSource 成功后失效该书的目录缓存', async () => {
    await getChapterList({ bookId: 'switch-book' })
    expect(getMock).toHaveBeenCalledTimes(1)
    await setBookSource({ bookId: 'switch-book', targetBookId: 'candidate-1' })
    expect(postMock).toHaveBeenCalledTimes(1)

    await getChapterList({ bookId: 'switch-book' })
    expect(getMock).toHaveBeenCalledTimes(2)
  })
})

