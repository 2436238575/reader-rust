// reader 核心 store 的用例：正文管线用例在 readerContent.test.ts，
// 评论/配图用例在 readerExtras.test.ts（reader store 拆分专项）；
// 这里保留核心自有的已编辑章清单用例。
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useReaderStore } from './reader'
import { listChapterEdits } from '../api/chapterEdit'

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

describe('reader core edited chapter keys', () => {
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
  })

  it('刷新编辑章清单只更新集合与快照，不再动浏览器缓存', async () => {
    // 缓存记录带版本号：是否串版本由在线读取时的比对兜底，清单刷新不承担清理
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
    // 快照落盘：离线打开时编辑按钮的状态靠它维持
    const raw = localStorage.getItem('reader-edited-chapters')
    expect(JSON.parse(raw || '{}').book_1?.urls).toEqual(['https://example.org/1'])
  })

  it('同一本书并发双触发只发一次 listChapterEdits（在途单飞）', async () => {
    let release!: (urls: string[]) => void
    vi.mocked(listChapterEdits).mockImplementation(
      () =>
        new Promise<string[]>((resolve) => {
          release = resolve
        })
    )
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }

    // 模拟开书双触发：loadBook 的 Promise.all 与 ReaderView onMounted 同时在途
    const first = readerStore.refreshEditedChapterKeys()
    const second = readerStore.refreshEditedChapterKeys()
    release(['https://example.org/1'])
    await Promise.all([first, second])

    expect(listChapterEdits).toHaveBeenCalledTimes(1)
    expect(readerStore.editedChapterKeys.has('https://example.org/1')).toBe(true)
  })

  it('在途单飞只限在途期间：落定后再次调用真的重新拉取', async () => {
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

    // 编辑保存/删除后的刷新依赖「落定后再调必重新拉」的语义
    await readerStore.refreshEditedChapterKeys()
    await readerStore.refreshEditedChapterKeys()

    expect(listChapterEdits).toHaveBeenCalledTimes(2)
  })

  it('拉取失败同样释放在途标记，下一次调用立即重试', async () => {
    vi.mocked(listChapterEdits).mockRejectedValueOnce(new Error('offline'))
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }

    // 失败被函数内吞掉（保持现状语义），但在途标记必须释放
    await readerStore.refreshEditedChapterKeys()
    await readerStore.refreshEditedChapterKeys()

    expect(listChapterEdits).toHaveBeenCalledTimes(2)
  })

  it('不同 bookId 的在途请求各自独立，互不合并', async () => {
    vi.mocked(listChapterEdits).mockResolvedValue([])
    const readerStore = useReaderStore()
    readerStore.book = {
      name: '网络书',
      author: '作者',
      origin: 'https://example.org',
      bookId: 'book_1',
      bookUrl: 'https://example.org/book',
      bookSourceId: 1,
    }
    const first = readerStore.refreshEditedChapterKeys()
    // 旧书请求在途期间换书：新书的刷新不能被合并到旧书的在途 Promise 上
    readerStore.book = {
      name: '另一本书',
      author: '作者',
      origin: 'https://other.example',
      bookId: 'book_2',
      bookUrl: 'https://other.example/b',
      bookSourceId: 2,
    }
    const second = readerStore.refreshEditedChapterKeys()
    await Promise.all([first, second])

    expect(listChapterEdits).toHaveBeenCalledTimes(2)
    expect(listChapterEdits).toHaveBeenCalledWith('book_1')
    expect(listChapterEdits).toHaveBeenCalledWith('book_2')
  })
})
