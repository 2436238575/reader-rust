// 正文管线（开书/取正文/预加载/换源/刷新）的用例：随 readerContent store 从
// reader.test.ts 迁出（reader store 拆分专项）。被测入口换成 contentStore，
// 状态种子仍打在 reader 核心 store 上（contentStore 内部读它）。
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useAppStore } from './app'
import { useReaderStore } from './reader'
import { useReaderContentStore } from './readerContent'
import { useReaderConfigStore } from './readerConfig'
import { getBookContent, getBookContents } from '../api/bookshelf'
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

describe('readerContent local txt chapters', () => {
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
    const contentStore = useReaderContentStore()
    appStore.setOnlineStatus(false)
    readerStore.book = {
      name: '本地书',
      author: '本地导入',
      origin: 'local-txt',
      bookId: 'book_1',
      bookUrl: 'local-txt:abc123',
    }
    readerStore.chapters = [{ title: '第一章', url: 'local-txt:abc123#0', index: 0 }]

    await expect(contentStore.fetchChapterContent(0)).resolves.toBe('本地正文')

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
    const contentStore = useReaderContentStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    readerStore.chapters = [{ title: '第一章', url: 'https://example.org/1', index: 0 }]
    // 阅读配置在 readerConfig store（拆分专项）
    useReaderConfigStore().updateConfig('contentVersion', 'upstream')

    await expect(contentStore.fetchChapterContent(0)).resolves.toBe('上游正文')

    expect(getBrowserCachedChapter).not.toHaveBeenCalled()
    expect(getBookContent).toHaveBeenCalledWith(expect.objectContaining({ variant: 'upstream' }))
    expect(readerStore.chapterVersions.get(0)?.kind).toBe('upstream')
    expect(readerStore.chapterDiffs.get(0)).toBeUndefined()
  })

  it('离线时编辑章不回退浏览器缓存，明确报需要联网', async () => {
    // 缓存里躺着编辑前的旧正文：一旦被读到就等于把另一台设备的编辑回退了
    vi.mocked(getBrowserCachedChapter).mockResolvedValue('编辑前的旧正文')
    const appStore = useAppStore()
    const readerStore = useReaderStore()
    const contentStore = useReaderContentStore()
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

    await expect(contentStore.fetchChapterContent(0)).rejects.toThrow('本章有编辑版，需要联网读取')
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
    const contentStore = useReaderContentStore()
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

    await expect(contentStore.fetchChapterContent(0)).resolves.toBe('编辑版正文')
    expect(setBrowserCachedChapter).not.toHaveBeenCalled()
  })

  it('loadBook 返回前清掉上一本书的编辑章集合并拉取本书集合', async () => {
    const { getChapterList } = await import('../api/bookshelf')
    vi.mocked(getChapterList).mockResolvedValue([
      { title: '第一章', url: 'https://example.org/1', index: 0 },
    ])
    const readerStore = useReaderStore()
    const contentStore = useReaderContentStore()
    readerStore.editedChapterKeys = new Set(['https://previous.example/old'])

    await contentStore.loadBook({
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
    const contentStore = useReaderContentStore()

    await contentStore.loadBook({
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
    const contentStore = useReaderContentStore()

    await contentStore.loadBook({
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
    const contentStore = useReaderContentStore()
    const serverSavedAt = 1_700_000_000_000

    await contentStore.loadBook({
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

    contentStore.clear()
    expect(readerStore.serverProgressTime).toBe(0)
  })
})

describe('readerContent preloadAroundChapter 批量预加载', () => {
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
    return useReaderContentStore()
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
    const contentStore = setupOnlineBook()

    await contentStore.preloadAroundChapter(1)

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
    await expect(contentStore.fetchChapterContent(2)).resolves.toBe('正文二')
    await expect(contentStore.fetchChapterContent(3)).resolves.toBe('正文三')
    await expect(contentStore.fetchChapterContent(0)).resolves.toBe('正文零')
    expect(getBookContent).not.toHaveBeenCalled()
    // 版本信息与单章路径同口径落账
    expect(useReaderStore().chapterVersions.get(2)?.selectionSource).toBe('upstream')
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
    const contentStore = setupOnlineBook()

    await contentStore.preloadAroundChapter(1)

    expect(getBookContents).toHaveBeenCalledTimes(1)
    // 只有失败的第 3 章走逐章路径；成功章（2/0）不重复请求
    expect(getBookContent).toHaveBeenCalledTimes(1)
    expect(getBookContent).toHaveBeenCalledWith(
      expect.objectContaining({ index: 3, chapterUrl: 'https://example.org/3' })
    )
    // 成功章与兜底章都已入预加载池：读到时命中，不再发单章请求
    await expect(contentStore.fetchChapterContent(2)).resolves.toBe('正文二')
    await expect(contentStore.fetchChapterContent(3)).resolves.toBe('正文3')
    await expect(contentStore.fetchChapterContent(0)).resolves.toBe('正文零')
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
    const contentStore = setupOnlineBook()

    await contentStore.preloadAroundChapter(1)

    // 第 3 章单章重试失败 → 走 getBrowserCachedChapter 兜底
    expect(getBookContent).toHaveBeenCalledTimes(1)
    expect(getBrowserCachedChapter).toHaveBeenCalledWith('book_1', 'https://example.org/3')
    await expect(contentStore.fetchChapterContent(3)).resolves.toBe('缓存正文三')
    // 成功章不受影响、不被重复请求
    await expect(contentStore.fetchChapterContent(2)).resolves.toBe('正文二')
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
    const contentStore = setupOnlineBook()

    await contentStore.preloadAroundChapter(1)

    expect(getBookContent).toHaveBeenCalledTimes(1)
    expect(getBookContent).toHaveBeenCalledWith(
      expect.objectContaining({ index: 3, chapterUrl: 'https://example.org/3' })
    )
    await expect(contentStore.fetchChapterContent(3)).resolves.toBe('正文3')
    await expect(contentStore.fetchChapterContent(2)).resolves.toBe('正文二')
    expect(getBookContent).toHaveBeenCalledTimes(1)
  })

  it('批量响应期间换书：旧结果丢弃且不回退逐章', async () => {
    const contentStore = setupOnlineBook()
    const readerStore = useReaderStore()
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

    await contentStore.preloadAroundChapter(1)

    expect(getBookContents).toHaveBeenCalledTimes(1)
    // 换书后既不算失败也不回退：不再发任何单章请求
    expect(getBookContent).not.toHaveBeenCalled()
  })

  it('整批失败时回退逐章 fetchChapterContent，行为与接入批量前一致', async () => {
    vi.mocked(getBookContents).mockRejectedValue(new Error('网络错误'))
    vi.mocked(getBookContent).mockImplementation(async (params) =>
      makeContentResponse(`正文${params.index}`)
    )
    const contentStore = setupOnlineBook()

    await contentStore.preloadAroundChapter(1)

    expect(getBookContents).toHaveBeenCalledTimes(1)
    // 逐章回退：3 个目标章各一次单章请求
    expect(getBookContent).toHaveBeenCalledTimes(3)
    await expect(contentStore.fetchChapterContent(2)).resolves.toBe('正文2')
    await expect(contentStore.fetchChapterContent(0)).resolves.toBe('正文0')
    expect(getBookContent).toHaveBeenCalledTimes(3)
  })
})
