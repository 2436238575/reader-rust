// 评论（章评/段评）与章节配图的用例：随 readerExtras store 从 reader.test.ts
// 迁出（reader store 拆分专项）。被测入口换成 extrasStore，状态种子仍打在
// reader 核心 store 上（extrasStore 内部读它）。
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useReaderStore } from './reader'
import { useReaderContentStore } from './readerContent'
import { useReaderExtrasStore } from './readerExtras'
import { getChapterImages } from '../api/bookshelf'
import { getChapterVersionMarks } from '../api/chapterEdit'
import { getChapterComments, getParaCommentIndex } from '../api/review'

vi.mock('../api/bookshelf', () => ({
  getChapterList: vi.fn(),
  getBookContent: vi.fn(),
  getBookContents: vi.fn(),
  getChapterImages: vi.fn(),
  saveBookProgress: vi.fn(),
  setBookSource: vi.fn(),
}))

vi.mock('../api/chapterEdit', () => ({
  getChapterVersionMarks: vi.fn(),
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
  getUserdataBatch: vi.fn().mockResolvedValue({}),
  saveUserdataBatch: vi.fn().mockResolvedValue(undefined),
}))

vi.mock('../utils/recentBooks', () => ({
  saveRecentReadBook: vi.fn(),
}))

vi.mock('../utils/openaiSpeech', () => ({
  DEFAULT_OPENAI_BASE_URL: 'https://api.openai.com/v1',
  requestOpenAISpeechAudio: vi.fn(),
}))

function seedBook(chapters: { title: string; url: string; index: number }[]) {
  const readerStore = useReaderStore()
  readerStore.book = {
    name: '网络书',
    author: '作者',
    origin: 'https://example.org',
    bookId: 'book_1',
    bookUrl: 'https://example.org/book',
    bookSourceId: 1,
  }
  readerStore.chapters = chapters
  return readerStore
}

describe('readerExtras 评论与配图', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    const storage = new Map<string, string>()
    vi.stubGlobal('localStorage', {
      getItem: vi.fn((key: string) => storage.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => storage.set(key, value)),
      removeItem: vi.fn((key: string) => storage.delete(key)),
      clear: vi.fn(() => storage.clear()),
    })
    vi.mocked(getChapterVersionMarks).mockReset().mockResolvedValue({})
    vi.mocked(getChapterComments).mockReset()
    vi.mocked(getParaCommentIndex).mockReset()
    // mockReset 连调用计数一起清：断言「翻章不再请求」不能带上前序用例的调用
    vi.mocked(getChapterImages).mockReset().mockResolvedValue({ enabled: false, images: [] })
  })

  it('activating a chapter reloads its images even when the content is already in hand', async () => {
    const readerStore = seedBook([{ title: '第一章', url: 'https://example.org/1', index: 0 }])
    // 配图由「章节激活信号 → readerContent 监听 → extras 拉取」驱动：
    // content store 必须先实例化（生产里任何管线入口都会先经过它）
    useReaderContentStore()
    useReaderExtrasStore()

    // 切正文版本/重载都走 setActiveChapterState；配图不能依赖调用方补
    readerStore.setActiveChapterState(0, '正文', 0)
    await vi.waitFor(() =>
      expect(getChapterImages).toHaveBeenCalledWith({
        bookId: 'book_1',
        chapterUrl: 'https://example.org/1',
      })
    )
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
    const readerStore = seedBook([
      { title: '第一章', url: 'https://example.org/1', index: 0 },
      { title: '第二章', url: 'https://example.org/2', index: 1 },
    ])
    const extrasStore = useReaderExtrasStore()
    readerStore.currentIndex = 0
    await extrasStore.loadChapterReviews(0)
    expect(getParaCommentIndex).toHaveBeenCalledTimes(1)
    expect(getChapterComments).toHaveBeenCalledTimes(1)
    expect(extrasStore.reviewEnabled).toBe(false)
    // enabled:false 时 data 原样写入（空页），翻章跳过时要清掉避免残留
    expect(extrasStore.chapterComments).not.toBeNull()

    readerStore.currentIndex = 1
    await extrasStore.loadChapterReviews(1)
    // 书源级结论记住后，两个评论接口都不再请求
    expect(getParaCommentIndex).toHaveBeenCalledTimes(1)
    expect(getChapterComments).toHaveBeenCalledTimes(1)
    expect(extrasStore.reviewEnabled).toBe(false)
    expect(extrasStore.paraReviewEnabled).toBe(false)
    expect(extrasStore.chapterComments).toBeNull()

    // 显式 refresh 绕过结论重新判断
    await extrasStore.loadChapterReviews(1, true)
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
    const readerStore = seedBook([
      { title: '第一章', url: 'https://example.org/1', index: 0 },
      { title: '第二章', url: 'https://example.org/2', index: 1 },
    ])
    const extrasStore = useReaderExtrasStore()
    readerStore.currentIndex = 0
    await extrasStore.loadChapterReviews(0)
    expect(getParaCommentIndex).toHaveBeenCalledTimes(1)
    expect(getChapterComments).toHaveBeenCalledTimes(1)

    readerStore.currentIndex = 1
    await extrasStore.loadChapterReviews(1)
    // 段评是独立规则：跳过段评，章评照常请求
    expect(getParaCommentIndex).toHaveBeenCalledTimes(1)
    expect(getChapterComments).toHaveBeenCalledTimes(2)
    expect(extrasStore.paraReviewEnabled).toBe(false)
    expect(extrasStore.paraReviewIndex).toEqual([])
    expect(extrasStore.reviewEnabled).toBe(true)
  })

  it('配图 enabled:false 的结论被记住，翻章不再重复请求', async () => {
    const readerStore = seedBook([
      { title: '第一章', url: 'https://example.org/1', index: 0 },
      { title: '第二章', url: 'https://example.org/2', index: 1 },
    ])
    const extrasStore = useReaderExtrasStore()
    readerStore.currentIndex = 0
    await extrasStore.loadChapterImages(0)
    expect(getChapterImages).toHaveBeenCalledTimes(1)

    readerStore.currentIndex = 1
    await extrasStore.loadChapterImages(1)
    // 书源没有配图规则是书源级结论：翻章不再请求，配图状态保持关闭
    expect(getChapterImages).toHaveBeenCalledTimes(1)
    expect(extrasStore.chapterImagesEnabled).toBe(false)
    expect(extrasStore.chapterImages).toEqual([])
  })
})
