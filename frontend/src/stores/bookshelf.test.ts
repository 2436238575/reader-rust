import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useBookshelfStore } from './bookshelf'
import { getBookshelfWithCacheInfo, addToShelf, removeFromShelf } from '../api/bookshelf'
import { deleteBrowserBookCache, listBrowserCacheSummary } from '../utils/browserCache'

vi.mock('../api/bookshelf', () => ({
  getBookshelfWithCacheInfo: vi.fn(),
  getBookGroups: vi.fn(),
  addToShelf: vi.fn(),
  removeFromShelf: vi.fn(),
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

describe('bookshelf shelf writes', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([])
    vi.mocked(listBrowserCacheSummary).mockResolvedValue([])
    vi.mocked(addToShelf).mockReset()
    vi.mocked(removeFromShelf).mockReset()
    vi.mocked(deleteBrowserBookCache).mockReset()
    vi.mocked(deleteBrowserBookCache).mockResolvedValue(undefined)
    // 计数断言要隔离：清掉前面用例累积的调用记录（保留实现）
    vi.mocked(getBookshelfWithCacheInfo).mockClear()
    vi.mocked(listBrowserCacheSummary).mockClear()
  })

  it('新书入架：PUT 201 后就地追加，不整架重拉', async () => {
    vi.mocked(addToShelf).mockResolvedValue({ bookId: 'book_new', created: true })
    const store = useBookshelfStore()

    const result = await store.addBook({
      bookId: 'book_new',
      bookSourceId: 7,
      name: '新书',
      author: '作者',
      bookUrl: 'https://book.example/1',
      origin: 'https://source.example',
    })

    expect(addToShelf).toHaveBeenCalledWith('book_new')
    expect(result.bookId).toBe('book_new')
    expect(store.books.map((book) => book.bookId)).toEqual(['book_new'])
    expect(store.books[0]).toMatchObject({
      name: '新书',
      author: '作者',
      bookUrl: 'https://book.example/1',
      origin: 'https://source.example',
      durChapterIndex: 0,
      browserCachedChapterCount: 0,
    })
    expect(getBookshelfWithCacheInfo).not.toHaveBeenCalled()
  })

  it('已在架（PUT 200）：服务端不重置进度，本地条目原样保留', async () => {
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([
      {
        bookId: 'book_a',
        name: '甲',
        author: '',
        bookUrl: 'https://b.example/a',
        origin: 'o',
        durChapterIndex: 12,
        durChapterPos: 3456,
        cachedChapterCount: 7,
      },
    ] as never)
    vi.mocked(listBrowserCacheSummary).mockResolvedValue([
      { bookId: 'book_a', cachedChapterCount: 5, bytes: 100, updatedAt: 1 },
    ])
    const store = useBookshelfStore()
    await store.fetchBooks()
    vi.mocked(getBookshelfWithCacheInfo).mockClear()

    vi.mocked(addToShelf).mockResolvedValue({ bookId: 'book_a', created: false })
    const result = await store.addBook({ bookId: 'book_a', name: '搜索结果名' })

    expect(result.name).toBe('甲')
    const kept = store.books.find((book) => book.bookId === 'book_a')
    expect(kept?.durChapterIndex).toBe(12)
    expect(kept?.cachedChapterCount).toBe(7)
    expect(kept?.browserCachedChapterCount).toBe(5)
    expect(getBookshelfWithCacheInfo).not.toHaveBeenCalled()
  })

  it('服务端已在架但本地缺条目（本地列表过期）：按传入字段补一条', async () => {
    vi.mocked(addToShelf).mockResolvedValue({ bookId: 'book_lost', created: false })
    const store = useBookshelfStore()

    await store.addBook({
      bookId: 'book_lost',
      name: '补登',
      author: '作者',
      bookUrl: 'https://b.example/lost',
      origin: 'o',
    })

    expect(store.books.map((book) => book.bookId)).toEqual(['book_lost'])
  })

  it('缺少 bookId 直接抛错，不发请求', async () => {
    const store = useBookshelfStore()
    await expect(store.addBook({ name: '无 id' })).rejects.toThrow('bookId')
    expect(addToShelf).not.toHaveBeenCalled()
  })

  it('入架失败原样抛错，书架保持不变', async () => {
    vi.mocked(addToShelf).mockRejectedValue(new Error('书架已满'))
    const store = useBookshelfStore()

    await expect(store.addBook({ bookId: 'book_x' })).rejects.toThrow('书架已满')

    expect(store.books).toEqual([])
    expect(getBookshelfWithCacheInfo).not.toHaveBeenCalled()
  })

  it('移出书架走 DELETE，并清理本地条目与浏览器缓存', async () => {
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([
      { bookId: 'book_a', name: '甲', author: '', bookUrl: 'https://b.example/a', origin: 'o' },
    ] as never)
    vi.mocked(removeFromShelf).mockResolvedValue(undefined)
    const store = useBookshelfStore()
    await store.fetchBooks()

    await store.removeBook(store.books[0])

    expect(removeFromShelf).toHaveBeenCalledWith('book_a')
    expect(deleteBrowserBookCache).toHaveBeenCalledWith('book_a')
    expect(store.books).toEqual([])
  })

  it('批量删除逐本调用 DELETE', async () => {
    vi.mocked(getBookshelfWithCacheInfo).mockResolvedValue([
      { bookId: 'book_a', name: '甲', author: '', bookUrl: 'https://b.example/a', origin: 'o' },
      { bookId: 'book_b', name: '乙', author: '', bookUrl: 'https://b.example/b', origin: 'o' },
    ] as never)
    vi.mocked(removeFromShelf).mockResolvedValue(undefined)
    const store = useBookshelfStore()
    await store.fetchBooks()
    store.toggleSelection('book_a')
    store.toggleSelection('book_b')

    await store.bulkDelete()

    expect(removeFromShelf).toHaveBeenCalledTimes(2)
    expect(store.books).toEqual([])
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
