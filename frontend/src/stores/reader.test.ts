// reader 核心 store 的用例：正文管线用例在 readerContent.test.ts，
// 评论/配图用例在 readerExtras.test.ts（reader store 拆分专项）；
// 这里保留核心自有的已编辑章清单用例。
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useReaderStore } from './reader'
import { listChapterEdits } from '../api/chapterEdit'
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
    vi.mocked(deleteBrowserCachedChapters).mockReset()
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
})
