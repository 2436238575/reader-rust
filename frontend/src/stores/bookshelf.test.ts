import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useBookshelfStore } from './bookshelf'
import { getBookshelfWithCacheInfo, saveBook } from '../api/bookshelf'
import { listBrowserCacheSummary } from '../utils/browserCache'

vi.mock('../api/bookshelf', () => ({
  getBookshelfWithCacheInfo: vi.fn(),
  getBookGroups: vi.fn(),
  deleteBook: vi.fn(),
  deleteBooks: vi.fn(),
  saveBook: vi.fn(),
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

describe('bookshelf fetchBooks cache counts', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([])
    vi.mocked(listBrowserCacheSummary).mockResolvedValue([])
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
})

describe('bookshelf addBook upsert', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([])
    vi.mocked(listBrowserCacheSummary).mockResolvedValue([])
    vi.mocked(saveBook).mockReset()
    // 计数断言要隔离：清掉前面用例累积的调用记录（保留实现）
    vi.mocked(getBookshelfWithCacheInfo).mockClear()
    vi.mocked(listBrowserCacheSummary).mockClear()
  })

  it('新书入架：追加 saveBook 返回的书籍，不整架重拉', async () => {
    const saved = {
      bookId: 'book_new',
      bookSourceId: 7,
      name: '新书',
      author: '作者',
      bookUrl: 'https://book.example/1',
      origin: 'https://source.example',
    }
    vi.mocked(saveBook).mockResolvedValue(saved as never)
    const store = useBookshelfStore()

    const result = await store.addBook({ bookId: 'book_new', name: '新书' })

    expect(result).toEqual(saved)
    expect(store.books.map((book) => book.bookId)).toEqual(['book_new'])
    expect(store.books[0].browserCachedChapterCount).toBe(0)
    // saveBook 响应不含缓存章数，新书不伪造该字段
    expect(store.books[0].cachedChapterCount).toBeUndefined()
    // 单本 upsert 的意义就是不再触发整架 GET
    expect(getBookshelfWithCacheInfo).not.toHaveBeenCalled()
  })

  it('已在架的书按 bookId 原位替换，保留前端注入的本地缓存章数', async () => {
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([
      { bookId: 'book_a', name: '甲', author: '', bookUrl: 'https://b.example/a', origin: 'o' },
      { bookId: 'book_b', name: '乙', author: '', bookUrl: 'https://b.example/b', origin: 'o' },
    ] as never)
    vi.mocked(listBrowserCacheSummary).mockResolvedValue([
      { bookId: 'book_b', cachedChapterCount: 5, bytes: 100, updatedAt: 1 },
    ])
    const store = useBookshelfStore()
    await store.fetchBooks()
    vi.mocked(getBookshelfWithCacheInfo).mockClear()

    const saved = {
      bookId: 'book_b',
      name: '乙（修订）',
      author: '某人',
      bookUrl: 'https://b.example/b',
      origin: 'o',
    }
    vi.mocked(saveBook).mockResolvedValue(saved as never)
    await store.addBook({ bookId: 'book_b' })

    // 原位替换：顺序不变、内容是 saveBook 的返回值
    expect(store.books.map((book) => book.bookId)).toEqual(['book_a', 'book_b'])
    const replaced = store.books.find((book) => book.bookId === 'book_b')
    expect(replaced?.name).toBe('乙（修订）')
    // saveBook 响应不带前端注入的本地缓存章数，替换时要留下旧值
    expect(replaced?.browserCachedChapterCount).toBe(5)
    expect(getBookshelfWithCacheInfo).not.toHaveBeenCalled()
  })

  it('已在架的书替换时保留服务端注入的远程缓存章数', async () => {
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([
      {
        bookId: 'book_a',
        name: '甲',
        author: '',
        bookUrl: 'https://b.example/a',
        origin: 'o',
        cachedChapterCount: 7,
      },
    ] as never)
    const store = useBookshelfStore()
    await store.fetchBooks()
    vi.mocked(getBookshelfWithCacheInfo).mockClear()

    // saveBook 响应不含 getShelfBookWithCacheInfo 注入的 cachedChapterCount
    const saved = {
      bookId: 'book_a',
      name: '甲（修订）',
      author: '某人',
      bookUrl: 'https://b.example/a',
      origin: 'o',
    }
    vi.mocked(saveBook).mockResolvedValue(saved as never)
    await store.addBook({ bookId: 'book_a' })

    const replaced = store.books.find((book) => book.bookId === 'book_a')
    expect(replaced?.name).toBe('甲（修订）')
    // 远程缓存章数 chip 不能在替换后消失
    expect(replaced?.cachedChapterCount).toBe(7)
    expect(getBookshelfWithCacheInfo).not.toHaveBeenCalled()
  })

  it('saveBook 响应带上远程缓存章数时以新值为准', async () => {
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([
      {
        bookId: 'book_a',
        name: '甲',
        author: '',
        bookUrl: 'https://b.example/a',
        origin: 'o',
        cachedChapterCount: 7,
      },
    ] as never)
    const store = useBookshelfStore()
    await store.fetchBooks()

    vi.mocked(saveBook).mockResolvedValue({
      bookId: 'book_a',
      name: '甲',
      author: '',
      bookUrl: 'https://b.example/a',
      origin: 'o',
      cachedChapterCount: 9,
    } as never)
    await store.addBook({ bookId: 'book_a' })

    expect(store.books.find((book) => book.bookId === 'book_a')?.cachedChapterCount).toBe(9)
  })

  it('saveBook 失败原样抛错，书架保持不变', async () => {
    vi.mocked(saveBook).mockRejectedValue(new Error('书架已满'))
    const store = useBookshelfStore()

    await expect(store.addBook({ bookId: 'book_x' })).rejects.toThrow('书架已满')

    expect(store.books).toEqual([])
    expect(getBookshelfWithCacheInfo).not.toHaveBeenCalled()
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
    const store = useBookshelfStore()

    // 先成功一轮把 dirty 清掉：模拟「书架本来干净，加书/换源后主动重拉」
    await store.fetchBooks()
    expect(store.shelfDirty).toBe(false)

    vi.mocked(getBookshelfWithCacheInfo).mockRejectedValue(new Error('网络错误'))
    await expect(store.fetchBooks()).rejects.toThrow('网络错误')

    // 这次失败必须重新标脏，否则返回书架时 onActivated 会跳过，
    // 新增/变更的书要等到下一次其他标脏操作才出现
    expect(store.shelfDirty).toBe(true)
  })
})
