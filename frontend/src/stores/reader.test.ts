// reader 核心 store 的用例：正文管线（开书/取正文/预加载）的用例已随
// readerContent store 迁到 readerContent.test.ts（reader store 拆分专项）；
// 这里保留核心自有的已编辑章清单、评论与章节配图用例。
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useReaderStore } from './reader'
import { getChapterImages } from '../api/bookshelf'
import { listChapterEdits } from '../api/chapterEdit'
import { getChapterComments, getParaCommentIndex } from '../api/review'
import { deleteBrowserCachedChapters } from '../utils/browserCache'

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

describe('reader core chapter state', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    const storage = new Map<string, string>()
    vi.stubGlobal('localStorage', {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
      clear: vi.fn(() => storage.clear()),
    })
    vi.mocked(listChapterEdits).mockReset().mockResolvedValue([])
    vi.mocked(deleteBrowserCachedChapters).mockReset()
    vi.mocked(getChapterComments).mockReset()
    vi.mocked(getParaCommentIndex).mockReset()
    // mockReset 连调用计数一起清：断言「翻章不再请求」不能带上前序用例的调用
    vi.mocked(getChapterImages).mockReset().mockResolvedValue({ enabled: false, images: [] })
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
