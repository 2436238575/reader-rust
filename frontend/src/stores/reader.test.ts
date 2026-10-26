import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useAppStore } from './app'
import { useReaderStore } from './reader'
import { getBookContent, getBookContents, getChapterImages } from '../api/bookshelf'
import type { ChapterContentResponse } from '../api/bookshelf'
import { listChapterEdits } from '../api/chapterEdit'
import { getChapterComments, getParaCommentIndex } from '../api/review'
import {
  deleteBrowserCachedChapters,
  getBrowserCachedChapter,
  setBrowserCachedChapter,
} from '../utils/browserCache'

vi.mock('../api/bookshelf', () => ({
  getChapterList: vi.fn(),
  getBookContent: vi.fn(),
  getBookContents: vi.fn(),
  getChapterImages: vi.fn(),
  saveBookProgress: vi.fn(),
  setBookSource: vi.fn(),
}))

vi.mock('../api/chapterEdit', () => ({
  listChapterEdits: vi.fn(),
  selectChapterVersion: vi.fn(),
}))

vi.mock('../api/review', () => ({
  getChapterComments: vi.fn(),
  getParaCommentIndex: vi.fn(),
  getParaComments: vi.fn(),
}))

vi.mock('../api/bookmark', () => ({
  getBookmarks: vi.fn(),
  saveBookmark: vi.fn(),
  deleteBookmark: vi.fn(),
  deleteBookmarks: vi.fn(),
}))

vi.mock('../api/replaceRule', () => ({
  getReplaceRules: vi.fn(),
}))

vi.mock('../utils/browserCache', () => ({
  getBrowserCachedChapter: vi.fn(),
  setBrowserCachedChapter: vi.fn(),
  deleteBrowserCachedChapter: vi.fn().mockResolvedValue(undefined),
  deleteBrowserCachedChapters: vi.fn().mockResolvedValue(undefined),
  getBrowserCatalog: vi.fn().mockResolvedValue(null),
  setBrowserCatalog: vi.fn().mockResolvedValue(undefined),
}))

vi.mock('../api/userdata', () => ({
  getUserdata: vi.fn().mockResolvedValue(null),
  saveUserdata: vi.fn().mockResolvedValue(undefined),
}))

vi.mock('../utils/recentBooks', () => ({
  saveRecentReadBook: vi.fn(),
}))

vi.mock('../utils/openaiSpeech', () => ({
  DEFAULT_OPENAI_BASE_URL: 'https://api.openai.com/v1',
  requestOpenAISpeechAudio: vi.fn(),
}))

describe('reader local txt chapters', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    const storage = new Map<string, string>()
    vi.stubGlobal('localStorage', {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
      clear: vi.fn(() => storage.clear()),
    })
    vi.mocked(getBookContent).mockReset()
    vi.mocked(getBookContents).mockReset()
    vi.mocked(getBrowserCachedChapter).mockReset()
    vi.mocked(setBrowserCachedChapter).mockReset()
    vi.mocked(listChapterEdits).mockReset().mockResolvedValue([])
    vi.mocked(deleteBrowserCachedChapters).mockReset()
    vi.mocked(getChapterComments).mockReset()
    vi.mocked(getParaCommentIndex).mockReset()
    // mockReset 连调用计数一起清：断言「翻章不再请求」不能带上前序用例的调用
    vi.mocked(getChapterImages).mockReset().mockResolvedValue({ enabled: false, images: [] })
  })

  it('fetches uploaded local txt content from backend even when browser reports offline', async () => {
    vi.mocked(getBookContent).mockResolvedValue({
      content: '本地正文',
      edited: false,
      variant: 'default',
      upstreamChanged: false,
      selectionSource: 'upstream',
      stale: false,
    })
    vi.mocked(getBrowserCachedChapter).mockResolvedValue(null)
    const appStore = useAppStore()
    const readerStore = useReaderStore()
    appStore.setOnlineStatus(false)
    readerStore.book = {
      name: '本地书',
      author: '本地导入',
      origin: 'local-txt',
      bookId: 'book_1',
      bookUrl: 'local-txt:abc123',
    }
    readerStore.chapters = [{ title: '第一章', url: 'local-txt:abc123#0', index: 0 }]

    await expect(readerStore.fetchChapterContent(0)).resolves.toBe('本地正文')

    expect(getBrowserCachedChapter).not.toHaveBeenCalled()
    expect(getBookContent).toHaveBeenCalledWith({
      chapterUrl: 'local-txt:abc123#0',
      bookId: 'book_1',
      index: 0,
      refresh: 0,
      variant: 'default',
    })
  })

  it('upstream variant skips browser cache and stores the version on the diff state', async () => {
    vi.mocked(getBookContent).mockResolvedValue({
      content: '上游正文',
      edited: false,
      variant: 'upstream',
      versionKind: 'upstream',
      upstreamChanged: false,
      selectionSource: 'upstream_preview',
      stale: false,
    })
    vi.mocked(getBrowserCachedChapter).mockResolvedValue(null)
    const appStore = useAppStore()
    appStore.setOnlineStatus(true)
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    readerStore.chapters = [{ title: '第一章', url: 'https://example.org/1', index: 0 }]
    readerStore.updateConfig('contentVersion', 'upstream')

    await expect(readerStore.fetchChapterContent(0)).resolves.toBe('上游正文')

    expect(getBrowserCachedChapter).not.toHaveBeenCalled()
    expect(getBookContent).toHaveBeenCalledWith(expect.objectContaining({ variant: 'upstream' }))
    expect(readerStore.chapterVersions.get(0)?.kind).toBe('upstream')
    expect(readerStore.chapterDiffs.get(0)).toBeUndefined()
  })

  it('activating a chapter reloads its images even when the content is already in hand', async () => {
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    readerStore.chapters = [{ title: '第一章', url: 'https://example.org/1', index: 0 }]

    // 切正文版本/重载都走 setActiveChapterState；配图不能依赖调用方补
    readerStore.setActiveChapterState(0, '正文', 0)
    await vi.waitFor(() =>
      expect(getChapterImages).toHaveBeenCalledWith({
        bookId: 'book_1',
        chapterUrl: 'https://example.org/1',
      })
    )
  })

  it('离线时编辑章不回退浏览器缓存，明确报需要联网', async () => {
    // 缓存里躺着编辑前的旧正文：一旦被读到就等于把另一台设备的编辑回退了
    vi.mocked(getBrowserCachedChapter).mockResolvedValue('编辑前的旧正文')
    const appStore = useAppStore()
    const readerStore = useReaderStore()
    appStore.setOnlineStatus(false)
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    readerStore.chapters = [{ title: '第一章', url: 'https://example.org/1', index: 0 }]
    readerStore.editedChapterKeys = new Set(['https://example.org/1'])

    await expect(readerStore.fetchChapterContent(0)).rejects.toThrow('本章有编辑版，需要联网读取')
    expect(getBrowserCachedChapter).not.toHaveBeenCalled()
  })

  it('在线读取编辑章时不写浏览器缓存', async () => {
    vi.mocked(getBookContent).mockResolvedValue({
      content: '编辑版正文',
      edited: true,
      variant: 'default',
      upstreamChanged: false,
      selectionSource: 'edited',
      stale: false,
    })
    vi.mocked(getBrowserCachedChapter).mockResolvedValue(null)
    const appStore = useAppStore()
    appStore.setOnlineStatus(true)
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    readerStore.chapters = [{ title: '第一章', url: 'https://example.org/1', index: 0 }]
    readerStore.editedChapterKeys = new Set(['https://example.org/1'])

    await expect(readerStore.fetchChapterContent(0)).resolves.toBe('编辑版正文')
    expect(setBrowserCachedChapter).not.toHaveBeenCalled()
  })

  it('刷新编辑章清单时删除新增编辑章的浏览器缓存', async () => {
    vi.mocked(listChapterEdits).mockResolvedValue(['https://example.org/1'])
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    readerStore.editedChapterKeys = new Set<string>()

    await readerStore.refreshEditedChapterKeys()

    expect(readerStore.editedChapterKeys.has('https://example.org/1')).toBe(true)
    expect(deleteBrowserCachedChapters).toHaveBeenCalledWith('book_1', ['https://example.org/1'])
  })

  it('loadBook 返回前清掉上一本书的编辑章集合并拉取本书集合', async () => {
    const { getChapterList } = await import('../api/bookshelf')
    vi.mocked(getChapterList).mockResolvedValue([
      { title: '第一章', url: 'https://example.org/1', index: 0 },
    ])
    const readerStore = useReaderStore()
    readerStore.editedChapterKeys = new Set(['https://previous.example/old'])

    await readerStore.loadBook({
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      durChapterIndex: 0,
    })

    expect(readerStore.editedChapterKeys.size).toBe(0)
    expect(listChapterEdits).toHaveBeenCalledWith('book_1')
  })

  it('离线开书时用上次落盘的编辑章集合，不把编辑章当普通章节', async () => {
    localStorage.setItem(
      'reader-edited-chapters',
      JSON.stringify({
        book_1: { urls: ['https://example.org/1'], savedAt: 1 },
      })
    )
    const { getChapterList } = await import('../api/bookshelf')
    vi.mocked(getChapterList).mockResolvedValue([
      { title: '第一章', url: 'https://example.org/1', index: 0 },
    ])
    // 离线：清单请求失败，内存集合只能来自上一次成功拉取时的落盘快照
    vi.mocked(listChapterEdits).mockRejectedValue(new Error('offline'))
    const readerStore = useReaderStore()

    await readerStore.loadBook({
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      durChapterIndex: 0,
    })

    expect(readerStore.editedChapterKeys.has('https://example.org/1')).toBe(true)
  })

  it('成功拉取编辑章清单后落盘，供下次离线打开使用', async () => {
    const { getChapterList } = await import('../api/bookshelf')
    vi.mocked(getChapterList).mockResolvedValue([
      { title: '第一章', url: 'https://example.org/1', index: 0 },
    ])
    vi.mocked(listChapterEdits).mockResolvedValue(['https://example.org/1'])
    const readerStore = useReaderStore()

    await readerStore.loadBook({
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      durChapterIndex: 0,
    })

    const raw = localStorage.getItem('reader-edited-chapters')
    expect(raw).not.toBeNull()
    expect(JSON.parse(raw || '{}').book_1?.urls).toEqual(['https://example.org/1'])
  })

  it('评论 enabled:false 的结论被记住，同书翻章不再重复请求', async () => {
    vi.mocked(getParaCommentIndex).mockResolvedValue({
      enabled: false,
      serverSort: false,
      data: { paras: [] },
    })
    vi.mocked(getChapterComments).mockResolvedValue({
      enabled: false,
      serverSort: false,
      data: { total: 0, hasMore: false, page: 1, items: [] },
    })
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    readerStore.chapters = [
      { title: '第一章', url: 'https://example.org/1', index: 0 },
      { title: '第二章', url: 'https://example.org/2', index: 1 },
    ]
    readerStore.currentIndex = 0
    await readerStore.loadChapterReviews(0)
    expect(getParaCommentIndex).toHaveBeenCalledTimes(1)
    expect(getChapterComments).toHaveBeenCalledTimes(1)
    expect(readerStore.reviewEnabled).toBe(false)
    // enabled:false 时 data 原样写入（空页），翻章跳过时要清掉避免残留
    expect(readerStore.chapterComments).not.toBeNull()

    readerStore.currentIndex = 1
    await readerStore.loadChapterReviews(1)
    // 书源级结论记住后，两个评论接口都不再请求
    expect(getParaCommentIndex).toHaveBeenCalledTimes(1)
    expect(getChapterComments).toHaveBeenCalledTimes(1)
    expect(readerStore.reviewEnabled).toBe(false)
    expect(readerStore.paraReviewEnabled).toBe(false)
    expect(readerStore.chapterComments).toBeNull()

    // 显式 refresh 绕过结论重新判断
    await readerStore.loadChapterReviews(1, true)
    expect(getParaCommentIndex).toHaveBeenCalledTimes(2)
    expect(getChapterComments).toHaveBeenCalledTimes(2)
  })

  it('只禁用段评时章评仍然请求', async () => {
    vi.mocked(getParaCommentIndex).mockResolvedValue({
      enabled: false,
      serverSort: false,
      data: { paras: [] },
    })
    vi.mocked(getChapterComments).mockResolvedValue({
      enabled: true,
      serverSort: false,
      data: { total: 1, hasMore: false, page: 1, items: [] },
    })
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    readerStore.chapters = [
      { title: '第一章', url: 'https://example.org/1', index: 0 },
      { title: '第二章', url: 'https://example.org/2', index: 1 },
    ]
    readerStore.currentIndex = 0
    await readerStore.loadChapterReviews(0)
    expect(getParaCommentIndex).toHaveBeenCalledTimes(1)
    expect(getChapterComments).toHaveBeenCalledTimes(1)

    readerStore.currentIndex = 1
    await readerStore.loadChapterReviews(1)
    // 段评是独立规则：跳过段评，章评照常请求
    expect(getParaCommentIndex).toHaveBeenCalledTimes(1)
    expect(getChapterComments).toHaveBeenCalledTimes(2)
    expect(readerStore.paraReviewEnabled).toBe(false)
    expect(readerStore.paraReviewIndex).toEqual([])
    expect(readerStore.reviewEnabled).toBe(true)
  })

  it('配图 enabled:false 的结论被记住，翻章不再重复请求', async () => {
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    readerStore.chapters = [
      { title: '第一章', url: 'https://example.org/1', index: 0 },
      { title: '第二章', url: 'https://example.org/2', index: 1 },
    ]
    readerStore.currentIndex = 0
    await readerStore.loadChapterImages(0)
    expect(getChapterImages).toHaveBeenCalledTimes(1)

    readerStore.currentIndex = 1
    await readerStore.loadChapterImages(1)
    // 书源没有配图规则是书源级结论：翻章不再请求，配图状态保持关闭
    expect(getChapterImages).toHaveBeenCalledTimes(1)
    expect(readerStore.chapterImagesEnabled).toBe(false)
    expect(readerStore.chapterImages).toEqual([])
  })
})

describe('reader progress conflict timestamps', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    const storage = new Map<string, string>()
    vi.stubGlobal('localStorage', {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
      clear: vi.fn(() => storage.clear()),
    })
  })

  it('loadBook 捕获服务端进度时间，且不被 setActiveChapterState 的 durChapterTime 刷新污染', async () => {
    const { getChapterList } = await import('../api/bookshelf')
    vi.mocked(getChapterList).mockResolvedValue([
      { title: '第一章', url: 'https://example.com/1', index: 0 },
    ])
    // loadBook 会并行拉编辑章清单：给个确定值，避免依赖别的 describe 的 mock 残留
    vi.mocked(listChapterEdits).mockResolvedValue([])
    const readerStore = useReaderStore()
    const serverSavedAt = 1_700_000_000_000

    await readerStore.loadBook({
      name: '书',
      author: '',
      origin: 'https://example.com',
      bookId: 'book_1',
      bookUrl: 'https://example.com/b',
      durChapterIndex: 0,
      durChapterTime: serverSavedAt,
    })
    expect(readerStore.serverProgressTime).toBe(serverSavedAt)

    // 打开章节会把 book.durChapterTime 刷成 now（书架排序需要）
    readerStore.setActiveChapterState(0, '正文', 0)
    expect(readerStore.book?.durChapterTime).not.toBe(serverSavedAt)
    // 但比较本地/服务端进度新旧用的 serverProgressTime 必须保持捕获值
    expect(readerStore.serverProgressTime).toBe(serverSavedAt)

    readerStore.clear()
    expect(readerStore.serverProgressTime).toBe(0)
  })
})

describe('reader preloadAroundChapter 批量预加载', () => {
  function makeContentResponse(content: string): ChapterContentResponse {
    return {
      content,
      edited: false,
      variant: 'default',
      upstreamChanged: false,
      selectionSource: 'upstream',
      stale: false,
    }
  }

  function setupOnlineBook() {
    const appStore = useAppStore()
    appStore.setOnlineStatus(true)
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    readerStore.chapters = [0, 1, 2, 3, 4].map((i) => ({
      title: `第${i}章`,
      url: `https://example.org/${i}`,
      index: i,
    }))
    return readerStore
  }

  beforeEach(() => {
    setActivePinia(createPinia())
    const storage = new Map<string, string>()
    vi.stubGlobal('localStorage', {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
      clear: vi.fn(() => storage.clear()),
    })
    vi.mocked(getBookContent).mockReset()
    vi.mocked(getBookContents).mockReset()
    vi.mocked(getBrowserCachedChapter).mockReset()
    // recordChapterContent 会 await 浏览器缓存写入：必须给 Promise，否则 .catch 抛 TypeError
    vi.mocked(setBrowserCachedChapter).mockReset().mockResolvedValue(undefined)
  })

  it('3 个目标章合成一次批量请求并全部写入预加载池', async () => {
    vi.mocked(getBookContents).mockResolvedValue([
      { index: 2, data: makeContentResponse('正文二'), error: null },
      { index: 3, data: makeContentResponse('正文三'), error: null },
      { index: 0, data: makeContentResponse('正文零'), error: null },
    ])
    const readerStore = setupOnlineBook()

    await readerStore.preloadAroundChapter(1)

    expect(getBookContents).toHaveBeenCalledTimes(1)
    expect(getBookContents).toHaveBeenCalledWith({
      bookId: 'book_1',
      chapters: [
        { index: 2, chapterUrl: 'https://example.org/2' },
        { index: 3, chapterUrl: 'https://example.org/3' },
        { index: 0, chapterUrl: 'https://example.org/0' },
      ],
      variant: 'default',
    })
    // 批量成功：不再逐章请求
    expect(getBookContent).not.toHaveBeenCalled()

    // 已写入 preloadedContent：读到这些章直接命中，不再发单章请求
    await expect(readerStore.fetchChapterContent(2)).resolves.toBe('正文二')
    await expect(readerStore.fetchChapterContent(3)).resolves.toBe('正文三')
    await expect(readerStore.fetchChapterContent(0)).resolves.toBe('正文零')
    expect(getBookContent).not.toHaveBeenCalled()
    // 版本信息与单章路径同口径落账
    expect(readerStore.chapterVersions.get(2)?.selectionSource).toBe('upstream')
  })

  it('批量响应里单章 error：只为失败章逐章兜底，成功章不重复请求', async () => {
    vi.mocked(getBookContents).mockResolvedValue([
      { index: 2, data: makeContentResponse('正文二'), error: null },
      { index: 3, data: null, error: '上游 403' },
      { index: 0, data: makeContentResponse('正文零'), error: null },
    ])
    vi.mocked(getBookContent).mockImplementation(async (params) =>
      makeContentResponse(`正文${params.index}`)
    )
    const readerStore = setupOnlineBook()

    await readerStore.preloadAroundChapter(1)

    expect(getBookContents).toHaveBeenCalledTimes(1)
    // 只有失败的第 3 章走逐章路径；成功章（2/0）不重复请求
    expect(getBookContent).toHaveBeenCalledTimes(1)
    expect(getBookContent).toHaveBeenCalledWith(
      expect.objectContaining({ index: 3, chapterUrl: 'https://example.org/3' })
    )
    // 成功章与兜底章都已入预加载池：读到时命中，不再发单章请求
    await expect(readerStore.fetchChapterContent(2)).resolves.toBe('正文二')
    await expect(readerStore.fetchChapterContent(3)).resolves.toBe('正文3')
    await expect(readerStore.fetchChapterContent(0)).resolves.toBe('正文零')
    expect(getBookContent).toHaveBeenCalledTimes(1)
  })

  it('失败章的逐章兜底自带浏览器缓存兜底：单章重试也失败时用缓存正文', async () => {
    vi.mocked(getBookContents).mockResolvedValue([
      { index: 2, data: makeContentResponse('正文二'), error: null },
      { index: 3, data: null, error: '上游 403' },
      { index: 0, data: makeContentResponse('正文零'), error: null },
    ])
    vi.mocked(getBookContent).mockRejectedValue(new Error('网络错误'))
    vi.mocked(getBrowserCachedChapter).mockImplementation(async (_bookId: string, url: string) =>
      url === 'https://example.org/3' ? '缓存正文三' : null
    )
    const readerStore = setupOnlineBook()

    await readerStore.preloadAroundChapter(1)

    // 第 3 章单章重试失败 → 走 getBrowserCachedChapter 兜底
    expect(getBookContent).toHaveBeenCalledTimes(1)
    expect(getBrowserCachedChapter).toHaveBeenCalledWith('book_1', 'https://example.org/3')
    await expect(readerStore.fetchChapterContent(3)).resolves.toBe('缓存正文三')
    // 成功章不受影响、不被重复请求
    await expect(readerStore.fetchChapterContent(2)).resolves.toBe('正文二')
    expect(getBookContent).toHaveBeenCalledTimes(1)
  })

  it('请求里有但响应缺失的章节同样逐章兜底', async () => {
    // 响应缺第 3 章（既无 data 也无 error 条目）
    vi.mocked(getBookContents).mockResolvedValue([
      { index: 2, data: makeContentResponse('正文二'), error: null },
      { index: 0, data: makeContentResponse('正文零'), error: null },
    ])
    vi.mocked(getBookContent).mockImplementation(async (params) =>
      makeContentResponse(`正文${params.index}`)
    )
    const readerStore = setupOnlineBook()

    await readerStore.preloadAroundChapter(1)

    expect(getBookContent).toHaveBeenCalledTimes(1)
    expect(getBookContent).toHaveBeenCalledWith(
      expect.objectContaining({ index: 3, chapterUrl: 'https://example.org/3' })
    )
    await expect(readerStore.fetchChapterContent(3)).resolves.toBe('正文3')
    await expect(readerStore.fetchChapterContent(2)).resolves.toBe('正文二')
    expect(getBookContent).toHaveBeenCalledTimes(1)
  })

  it('批量响应期间换书：旧结果丢弃且不回退逐章', async () => {
    const readerStore = setupOnlineBook()
    vi.mocked(getBookContents).mockImplementation(async () => {
      // 模拟响应期间换书：旧请求的结果不能写进新书的预加载池
      readerStore.book = {
        name: '另一本书',
        author: '',
        origin: 'https://other.example',
        bookId: 'book_2',
        bookUrl: 'https://other.example/b',
      }
      return [
        { index: 2, data: makeContentResponse('旧书正文'), error: null },
        { index: 3, data: null, error: '上游 403' },
      ]
    })

    await readerStore.preloadAroundChapter(1)

    expect(getBookContents).toHaveBeenCalledTimes(1)
    // 换书后既不算失败也不回退：不再发任何单章请求
    expect(getBookContent).not.toHaveBeenCalled()
  })

  it('整批失败时回退逐章 fetchChapterContent，行为与接入批量前一致', async () => {
    vi.mocked(getBookContents).mockRejectedValue(new Error('网络错误'))
    vi.mocked(getBookContent).mockImplementation(async (params) =>
      makeContentResponse(`正文${params.index}`)
    )
    const readerStore = setupOnlineBook()

    await readerStore.preloadAroundChapter(1)

    expect(getBookContents).toHaveBeenCalledTimes(1)
    // 逐章回退：3 个目标章各一次单章请求
    expect(getBookContent).toHaveBeenCalledTimes(3)
    await expect(readerStore.fetchChapterContent(2)).resolves.toBe('正文2')
    await expect(readerStore.fetchChapterContent(0)).resolves.toBe('正文0')
    expect(getBookContent).toHaveBeenCalledTimes(3)
  })
})

