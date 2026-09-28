import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useExploreStore } from './explore'
import { useSourceStore } from './source'
import { getBookSources } from '../api/source'
import { exploreBook } from '../api/explore'
import type { BookSource, SearchBook } from '../types'

vi.mock('../api/source', () => ({
  getBookSources: vi.fn(),
}))

vi.mock('../api/explore', () => ({
  exploreBook: vi.fn(),
}))

const getBookSourcesMock = vi.mocked(getBookSources)
const exploreBookMock = vi.mocked(exploreBook)

describe('explore store source sync', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    getBookSourcesMock.mockReset()
    exploreBookMock.mockReset()
    exploreBookMock.mockResolvedValue([])
  })

  it('repairs a stale active source when explore sources are already loaded', async () => {
    const sourceStore = useSourceStore()
    sourceStore.sources = [sourceWithExplore()]
    const store = useExploreStore()
    store.activeSourceUrl = 'https://missing.example'

    await store.init()

    expect(store.activeSourceUrl).toBe('https://m.cuoceng.com')
    expect(store.categories.map((category) => category.title)).toEqual(['书 库', '排 行'])
    expect(store.activeCategoryUrl).toBe('/book/category/catalog.html')
  })

  it('加载中切换分类时，旧分类的迟到响应被丢弃且不污染 loading', async () => {
    const sourceStore = useSourceStore()
    sourceStore.sources = [sourceWithExplore()]
    const store = useExploreStore()
    await store.init()
    exploreBookMock.mockClear()  // init 自己会拉一页，清掉计数再观察竞态

    let resolveOld!: (books: SearchBook[]) => void
    let resolveNew!: (books: SearchBook[]) => void
    exploreBookMock
      .mockImplementationOnce(() => new Promise((r) => { resolveOld = r }))
      .mockImplementationOnce(() => new Promise((r) => { resolveNew = r }))

    store.setCategory('/book/ranking.html')
    await vi.waitFor(() => expect(exploreBookMock).toHaveBeenCalledTimes(1))
    store.setCategory('/book/category/catalog.html')
    await vi.waitFor(() => expect(exploreBookMock).toHaveBeenCalledTimes(2))

    // 旧分类（排行）的响应迟到：数据必须被丢弃，新请求的 loading 不受影响
    resolveOld([{ name: '旧分类的书', author: '', bookUrl: 'old', origin: '' }])
    await new Promise((r) => setTimeout(r, 0))
    expect(store.books).toEqual([])
    expect(store.loading).toBe(true)

    resolveNew([{ name: '新分类的书', author: '', bookUrl: 'new', origin: '' }])
    await vi.waitFor(() => expect(store.books.length).toBe(1))
    expect(store.books[0].name).toBe('新分类的书')
    expect(store.loading).toBe(false)
  })

  it('加载失败后可通过 retryFetch 恢复', async () => {
    const sourceStore = useSourceStore()
    sourceStore.sources = [sourceWithExplore()]
    const store = useExploreStore()
    await store.init()

    exploreBookMock.mockRejectedValueOnce(new Error('网络错误'))
    store.setCategory('/book/ranking.html')
    await vi.waitFor(() => expect(store.error).toBe('网络错误'))
    expect(store.hasMore).toBe(false)

    exploreBookMock.mockResolvedValueOnce([{ name: '恢复的书', author: '', bookUrl: 'b1', origin: '' }])
    store.retryFetch()
    await vi.waitFor(() => expect(store.books.length).toBe(1))
    expect(store.error).toBeNull()
  })

  it('init 拉取书源失败时写入 error 而不是抛出', async () => {
    getBookSourcesMock.mockRejectedValueOnce(new Error('服务器开小差'))
    const store = useExploreStore()
    await store.init()
    expect(store.error).toBe('服务器开小差')
  })
})

function sourceWithExplore(): BookSource {
  return {
    bookSourceName: 'm.cuoceng.com',
    bookSourceUrl: 'https://m.cuoceng.com',
    enabledExplore: true,
    exploreUrl: JSON.stringify([
      {
        style: { layout_flexBasisPercent: 1.0, layout_flexGrow: 1 },
        title: '书 库',
        url: '/book/category/catalog.html',
      },
      {
        style: { layout_flexBasisPercent: 0.25, layout_flexGrow: 1 },
        title: '排 行',
        url: '/book/ranking.html',
      },
    ]),
  }
}
