import { beforeEach, describe, expect, it, vi } from 'vitest'
import { nextTick, watch } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import { useExploreStore } from './explore'
import { useSourceStore } from './source'
import { getBookSourceList } from '../api/source'
import { exploreBook } from '../api/explore'
import type { BookSource, SearchBook } from '../types'

vi.mock('../api/source', () => ({
  getBookSources: vi.fn(),
  getBookSourceList: vi.fn(),
  saveBookSource: vi.fn(),
  saveBookSources: vi.fn(),
  deleteBookSource: vi.fn(),
  deleteBookSources: vi.fn(),
  deleteInvalidBookSources: vi.fn(),
}))

vi.mock('../api/explore', () => ({
  exploreBook: vi.fn(),
}))

const getBookSourceListMock = vi.mocked(getBookSourceList)
const exploreBookMock = vi.mocked(exploreBook)

describe('explore store source sync', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    getBookSourceListMock.mockReset()
    exploreBookMock.mockReset()
    exploreBookMock.mockResolvedValue([])
  })

  it('repairs a stale active source when explore sources are already loaded', async () => {
    const sourceStore = useSourceStore()
    sourceStore.sources = [sourceWithExplore()]
    const store = useExploreStore()
    store.activeSourceId = 999 // 已不存在的书源 id

    await store.init()

    expect(store.activeSourceId).toBe(1)
    expect(store.categories.map((category) => category.title)).toEqual(['书 库', '排 行'])
    expect(store.activeCategoryUrl).toBe('/book/category/catalog.html')
  })

  it('加载中切换分类时，旧分类的迟到响应被丢弃且不污染 loading', async () => {
    const sourceStore = useSourceStore()
    sourceStore.sources = [sourceWithExplore()]
    const store = useExploreStore()
    await store.init()
    exploreBookMock.mockClear() // init 自己会拉一页，清掉计数再观察竞态

    let resolveOld!: (books: SearchBook[]) => void
    let resolveNew!: (books: SearchBook[]) => void
    exploreBookMock
      .mockImplementationOnce(
        () =>
          new Promise((r) => {
            resolveOld = r
          })
      )
      .mockImplementationOnce(
        () =>
          new Promise((r) => {
            resolveNew = r
          })
      )

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

    exploreBookMock.mockResolvedValueOnce([
      { name: '恢复的书', author: '', bookUrl: 'b1', origin: '' },
    ])
    store.retryFetch()
    await vi.waitFor(() => expect(store.books.length).toBe(1))
    expect(store.error).toBeNull()
  })

  it('init 拉取书源失败时写入 error 而不是抛出', async () => {
    getBookSourceListMock.mockRejectedValueOnce(new Error('服务器开小差'))
    const store = useExploreStore()
    await store.init()
    expect(store.error).toBe('服务器开小差')
  })

  it('书源处于快照占位（partial）时，init 等摘要就位再选源', async () => {
    const sourceStore = useSourceStore()
    // 快照里的书源没有 exploreUrl：直接据此选源会得出「无带有发现规则的书源」
    sourceStore.sources = [
      { bookSourceId: 1, bookSourceName: '快照源', bookSourceUrl: 'https://example.test' },
    ]
    sourceStore.partial = true
    getBookSourceListMock.mockResolvedValueOnce([sourceWithExplore()])
    const store = useExploreStore()

    await store.init()

    expect(getBookSourceListMock).toHaveBeenCalledTimes(1)
    expect(store.error).toBeNull()
    expect(store.activeSourceId).toBe(1)
    expect(store.activeCategoryUrl).toBe('/book/category/catalog.html')
  })

  // 以下两条守护「误请求」回归：store 实例化本身不得带任何自动请求副作用，
  // 书源列表变化的观察由发现页在激活期间自行挂载（ExploreView onActivated/onDeactivated）

  it('只实例化 store、不启动观察时，书源列表变化不会调用 exploreBook', async () => {
    const sourceStore = useSourceStore()
    const store = useExploreStore()

    sourceStore.sources = [sourceWithExplore()]
    await nextTick()
    await nextTick()

    expect(exploreBookMock).not.toHaveBeenCalled()
    // 没有观察者就不会自动选中书源
    expect(store.activeSourceId).toBeNull()
  })

  it('启动观察后书源变化会触发请求，停止观察后不再触发', async () => {
    const sourceStore = useSourceStore()
    const store = useExploreStore()

    // 与 ExploreView 激活期间的接线一致：观察 exploreSources，变化时对账
    const stop = watch(
      () => store.exploreSources,
      () => store.ensureActiveSource()
    )

    sourceStore.sources = [sourceWithExplore()]
    await vi.waitFor(() => expect(exploreBookMock).toHaveBeenCalledTimes(1))
    expect(store.activeSourceId).toBe(1)

    stop()
    exploreBookMock.mockClear()

    // 换成首分类不同的书源（id=2）：若观察仍在，会切源并发起新请求
    sourceStore.sources = [sourceWithExplore(2, '/book/other.html')]
    await nextTick()
    await nextTick()

    expect(exploreBookMock).not.toHaveBeenCalled()
    // 停止观察后书源变化不再对账，选中状态保持不变
    expect(store.activeSourceId).toBe(1)
  })
})

function sourceWithExplore(
  bookSourceId = 1,
  firstCategoryUrl = '/book/category/catalog.html'
): BookSource {
  return {
    bookSourceId,
    bookSourceName: 'm.cuoceng.com',
    bookSourceUrl: 'https://m.cuoceng.com',
    enabledExplore: true,
    exploreUrl: JSON.stringify([
      {
        style: { layout_flexBasisPercent: 1.0, layout_flexGrow: 1 },
        title: '书 库',
        url: firstCategoryUrl,
      },
      {
        style: { layout_flexBasisPercent: 0.25, layout_flexGrow: 1 },
        title: '排 行',
        url: '/book/ranking.html',
      },
    ]),
  }
}
