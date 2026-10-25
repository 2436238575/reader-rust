import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useBookshelfStore } from './bookshelf'
import { getBookshelfWithCacheInfo } from '../api/bookshelf'
import { listBrowserCacheSummary } from '../utils/browserCache'

vi.mock('../api/bookshelf', () => ({
  getBookshelfWithCacheInfo: vi.fn(),
  getBookGroups: vi.fn(),
  deleteBook: vi.fn(),
  deleteBooks: vi.fn(),
  saveBookGroupId: vi.fn(),
  saveBookGroup: vi.fn(),
  deleteBookGroup: vi.fn(),
  saveBooks: vi.fn(),
}))

vi.mock('../utils/browserCache', () => ({
  deleteBrowserBookCache: vi.fn(),
  listBrowserCacheSummary: vi.fn(),
}))

vi.mock('../utils/recentBooks', () => ({
  clearRecentReadBooks: vi.fn(),
  getRecentReadBookKey: vi.fn((book) => book.bookId || ''),
  loadRecentReadBooks: vi.fn(() => []),
  removeRecentReadBook: vi.fn(),
  syncRecentBooksFromBackend: vi.fn(async () => undefined),
}))

describe('bookshelf search state', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([])
    vi.mocked(listBrowserCacheSummary).mockResolvedValue([])
  })

  it('starts searches in single-source scope by default', () => {
    const store = useBookshelfStore()

    store.startSearch('星门')

    expect(store.searchKey).toBe('星门')
    expect(store.searchScope).toBe('source')
    expect(store.searchSourceId).toBeNull()
    expect(store.searchGroup).toBe('')
  })

  it('does not display browser cache counts for uploaded local txt books', async () => {
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([
      {
        bookId: 'book-local',
        name: '本地书',
        author: '本地导入',
        origin: 'local-txt',
        bookUrl: 'local-txt:abc',
        cachedChapterCount: 12,
      },
      {
        bookId: 'book-remote',
        name: '远程书',
        author: '作者',
        origin: 'https://source.example',
        bookUrl: 'https://book.example/1',
      },
    ] as never)
    vi.mocked(listBrowserCacheSummary).mockResolvedValue([
      { bookId: 'book-local', cachedChapterCount: 12, bytes: 100, updatedAt: 1 },
      { bookId: 'book-remote', cachedChapterCount: 3, bytes: 200, updatedAt: 2 },
    ])
    const store = useBookshelfStore()

    await store.fetchBooks()

    expect(
      store.books.find((book) => book.bookId === 'book-local')?.browserCachedChapterCount
    ).toBe(0)
    expect(
      store.books.find((book) => book.bookId === 'book-remote')?.browserCachedChapterCount
    ).toBe(3)
  })

  it('can start a search with the active explore source selected', () => {
    const store = useBookshelfStore()

    store.startSearch('星门', { sourceId: 42 })

    expect(store.searchKey).toBe('星门')
    expect(store.searchScope).toBe('source')
    expect(store.searchSourceId).toBe(42)
  })
})

describe('bookshelf dirty flag and fetch dedup', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([])
    vi.mocked(listBrowserCacheSummary).mockResolvedValue([])
    // 计数断言要隔离：清掉前面用例累积的调用记录（保留实现）
    vi.mocked(getBookshelfWithCacheInfo).mockClear()
    vi.mocked(listBrowserCacheSummary).mockClear()
  })

  it('merges concurrent fetchBooks calls into a single round of requests', async () => {
    let resolveShelf: (value: unknown) => void = () => {}
    vi.mocked(getBookshelfWithCacheInfo).mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveShelf = resolve
        }) as never
    )
    const store = useBookshelfStore()

    const first = store.fetchBooks()
    const second = store.fetchBooks()
    resolveShelf([])
    await Promise.all([first, second])

    expect(getBookshelfWithCacheInfo).toHaveBeenCalledTimes(1)
    // listBrowserCacheSummary 一轮 fetch 里被读两次（fetchBooks 本体 + 内部的 refreshRecentBooks）；
    // 若并发未被合并会是 4 次
    expect(listBrowserCacheSummary).toHaveBeenCalledTimes(2)
  })

  it('clears the dirty flag after a successful fetch', async () => {
    const store = useBookshelfStore()
    expect(store.shelfDirty).toBe(true) // 初始为脏：首载必拉

    await store.fetchBooks()

    expect(store.shelfDirty).toBe(false)
  })

  it('keeps the shelf dirty when marked while a fetch is in flight', async () => {
    let resolveShelf: (value: unknown) => void = () => {}
    vi.mocked(getBookshelfWithCacheInfo).mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveShelf = resolve
        }) as never
    )
    const store = useBookshelfStore()

    const pending = store.fetchBooks()
    store.markShelfDirty() // fetch 进行中书架又被标脏（如阅读进度变化）
    resolveShelf([])
    await pending

    // 代数已变：本轮成功不能清掉后来发生的标脏
    expect(store.shelfDirty).toBe(true)
  })

  it('keeps the shelf dirty when a fetch fails', async () => {
    vi.mocked(getBookshelfWithCacheInfo).mockRejectedValue(new Error('网络错误'))
    const store = useBookshelfStore()

    await expect(store.fetchBooks()).rejects.toThrow('网络错误')

    expect(store.shelfDirty).toBe(true)
  })
})
