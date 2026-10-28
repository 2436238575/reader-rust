// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import {
  useSourceStore,
  SOURCE_SNAPSHOT_KEY,
  SOURCE_SNAPSHOT_FIELDS,
} from './source'
import {
  getBookSourceList,
  saveBookSource,
  saveBookSources,
  deleteBookSource,
  deleteBookSources,
  deleteInvalidBookSources,
} from '../api/source'
import type { BookSource, BookSourceSummary } from '../types'

vi.mock('../api/source', () => ({
  getBookSources: vi.fn(),
  getBookSourceList: vi.fn(),
  getBookSource: vi.fn(),
  saveBookSource: vi.fn(),
  saveBookSources: vi.fn(),
  deleteBookSource: vi.fn(),
  deleteBookSources: vi.fn(),
  deleteInvalidBookSources: vi.fn(),
}))

const getBookSourceListMock = vi.mocked(getBookSourceList)
const saveBookSourceMock = vi.mocked(saveBookSource)
const saveBookSourcesMock = vi.mocked(saveBookSources)
const deleteBookSourceMock = vi.mocked(deleteBookSource)
const deleteBookSourcesMock = vi.mocked(deleteBookSources)
const deleteInvalidBookSourcesMock = vi.mocked(deleteInvalidBookSources)

/** 同时带齐敏感字段（header/loginUrl/jsLib/规则等）的完整书源：快照绝不能带上它们 */
function fullSource(overrides: Partial<BookSource> = {}): BookSource {
  return {
    bookSourceId: 1,
    bookSourceName: '测试源',
    bookSourceUrl: 'https://example.test',
    bookSourceGroup: '默认',
    bookSourceType: 0,
    enabled: true,
    enabledExplore: true,
    lastUpdateTime: 1700000000000,
    respondTime: 120,
    customOrder: 3,
    weight: 10,
    header: 'secret-header-value',
    loginUrl: 'https://example.test/login',
    loginUi: '[]',
    loginCheckJs: 'check-js',
    jsLib: 'js-lib-content',
    coverDecodeJs: 'cover-decode',
    searchUrl: 'https://example.test/search',
    exploreUrl: 'https://example.test/explore',
    bookUrlPattern: 'https://example.test/book/\\d+',
    enabledCookieJar: true,
    concurrentRate: '12',
    exploreScreen: 'screen',
    bookSourceComment: 'comment',
    variableComment: 'variable',
    loadWithBaseUrl: true,
    singleUrl: false,
    ruleSearch: { name: '.name', bookUrl: '.url' },
    ruleExplore: { name: '.e-name' },
    ruleBookInfo: { name: '.b-name' },
    ruleToc: { chapterName: '.c-name', chapterList: '.c-list' },
    ruleContent: { content: '.content' },
    ruleReview: { reviewUrl: 'https://example.test/review' },
    ...overrides,
  }
}

function seedSnapshot(sources: BookSource[], savedAt = Date.now()) {
  localStorage.setItem(SOURCE_SNAPSHOT_KEY, JSON.stringify({ savedAt, sources }))
}

function readSnapshotRaw(): { savedAt: number; sources: Record<string, unknown>[] } {
  const raw = localStorage.getItem(SOURCE_SNAPSHOT_KEY)
  expect(raw).toBeTruthy()
  return JSON.parse(raw!)
}

beforeEach(() => {
  setActivePinia(createPinia())
  localStorage.clear()
  vi.clearAllMocks()
})

describe('source store 冷启动快照', () => {
  it('摘要加载成功后写快照，且快照只含白名单字段', async () => {
    getBookSourceListMock.mockResolvedValue([fullSource()])
    const store = useSourceStore()

    await store.fetchSources()

    expect(store.partial).toBe(false)
    const snapshot = readSnapshotRaw()
    expect(snapshot.sources).toHaveLength(1)
    // 每条快照的键严格等于白名单（白名单字段全都有值时）
    expect([...Object.keys(snapshot.sources[0])].sort()).toEqual(
      [...SOURCE_SNAPSHOT_FIELDS].sort()
    )
    // 敏感字段的值不出现在快照文本里（exploreUrl 也只进本地列表摘要，不落快照）
    const raw = localStorage.getItem(SOURCE_SNAPSHOT_KEY)!
    for (const leaked of [
      'secret-header-value',
      'login',
      'js-lib-content',
      'cover-decode',
      'ruleSearch',
      'searchUrl',
      'exploreUrl',
      'ruleContent',
      'bookUrlPattern',
    ]) {
      expect(raw).not.toContain(leaked)
    }
  })

  it('新鲜快照先渲染（partial=true）并立即返回，后台摘要成功后替换（partial=false）并重写快照', async () => {
    seedSnapshot([fullSource({ bookSourceId: 1, bookSourceName: '快照源' })])
    let resolveList!: (list: BookSourceSummary[]) => void
    getBookSourceListMock.mockImplementation(
      () => new Promise<BookSourceSummary[]>((resolve) => (resolveList = resolve))
    )
    const store = useSourceStore()

    await store.fetchSources()

    // 立即返回时已是快照内容；快照对象被消毒过，不带敏感字段
    expect(store.partial).toBe(true)
    expect(store.sources.map((s) => s.bookSourceName)).toEqual(['快照源'])
    expect(store.sources[0]).not.toHaveProperty('header')
    expect(store.sources[0]).not.toHaveProperty('ruleSearch')
    expect(getBookSourceListMock).toHaveBeenCalledTimes(1) // 后台摘要在途

    resolveList([fullSource({ bookSourceId: 2, bookSourceName: '摘要源' })])
    await vi.waitFor(() => expect(store.partial).toBe(false))

    expect(store.sources.map((s) => s.bookSourceName)).toEqual(['摘要源'])
    expect(readSnapshotRaw().sources.map((s) => s.bookSourceName)).toEqual(['摘要源'])
  })

  it('后台摘要失败时保留快照（partial 保持 true）', async () => {
    seedSnapshot([fullSource({ bookSourceName: '快照源' })])
    getBookSourceListMock.mockRejectedValue(new Error('network down'))
    const store = useSourceStore()

    await store.fetchSources()

    expect(store.partial).toBe(true)
    await vi.waitFor(() => expect(store.loading).toBe(false))
    expect(store.partial).toBe(true)
    expect(store.sources.map((s) => s.bookSourceName)).toEqual(['快照源'])
  })

  it('过期快照（超过 24h）不命中，按普通摘要加载', async () => {
    seedSnapshot([fullSource({ bookSourceName: '过期快照' })], Date.now() - 25 * 3600 * 1000)
    getBookSourceListMock.mockResolvedValue([fullSource({ bookSourceName: '摘要源' })])
    const store = useSourceStore()

    await store.fetchSources()

    expect(getBookSourceListMock).toHaveBeenCalledTimes(1)
    expect(store.partial).toBe(false)
    expect(store.sources.map((s) => s.bookSourceName)).toEqual(['摘要源'])
  })

  it('损坏的快照 JSON 被忽略，按普通摘要加载', async () => {
    localStorage.setItem(SOURCE_SNAPSHOT_KEY, '{not-json')
    getBookSourceListMock.mockResolvedValue([fullSource({ bookSourceName: '摘要源' })])
    const store = useSourceStore()

    await store.fetchSources()

    expect(store.partial).toBe(false)
    expect(store.sources.map((s) => s.bookSourceName)).toEqual(['摘要源'])
  })

  it('超限（>1000 条）的快照不命中，按普通摘要加载', async () => {
    const oversized = Array.from({ length: 1001 }, (_, i) =>
      fullSource({ bookSourceId: i + 1, bookSourceName: `源${i}` })
    )
    seedSnapshot(oversized)
    getBookSourceListMock.mockResolvedValue([fullSource({ bookSourceName: '摘要源' })])
    const store = useSourceStore()

    await store.fetchSources()

    expect(store.partial).toBe(false)
    expect(store.sources.map((s) => s.bookSourceName)).toEqual(['摘要源'])
  })

  it('fetchSources({force:true}) 忽略新鲜快照直接拉摘要', async () => {
    seedSnapshot([fullSource({ bookSourceName: '快照源' })])
    getBookSourceListMock.mockResolvedValue([fullSource({ bookSourceName: '摘要源' })])
    const store = useSourceStore()

    await store.fetchSources({ force: true })

    expect(store.partial).toBe(false)
    expect(store.sources.map((s) => s.bookSourceName)).toEqual(['摘要源'])
  })
})

describe('source store mutation', () => {
  it('saveSource 立即保存（列表只存摘要，无列表依赖），成功后按摘要 upsert 且不整表重拉', async () => {
    getBookSourceListMock.mockResolvedValue([fullSource({ bookSourceId: 1, bookSourceName: '摘要A' })])
    saveBookSourceMock.mockResolvedValue({ saved: true, bookSourceId: 2 })
    const store = useSourceStore()
    await store.fetchSources()

    const updated = fullSource({ bookSourceId: 2, bookSourceName: '改名B', exploreUrl: '榜单::/x' })
    await store.saveSource(updated)

    expect(saveBookSourceMock).toHaveBeenCalledWith(updated)
    expect(getBookSourceListMock).toHaveBeenCalledTimes(1) // 局部更新，不重拉
    const saved = store.sources.find((s) => s.bookSourceId === 2)
    expect(saved?.bookSourceName).toBe('改名B')
    expect(saved?.exploreUrl).toBe('榜单::/x')
    // 摘要不带规则/敏感字段
    expect(saved).not.toHaveProperty('ruleSearch')
    expect(saved).not.toHaveProperty('header')
    expect(store.sources.find((s) => s.bookSourceId === 1)?.bookSourceName).toBe('摘要A')
    // 快照同步重写，且不含敏感字段
    const snapshot = readSnapshotRaw()
    expect(snapshot.sources.find((s) => s.bookSourceId === 2)?.bookSourceName).toBe('改名B')
    expect(localStorage.getItem(SOURCE_SNAPSHOT_KEY)!).not.toContain('secret-header-value')
  })

  it('saveSource 新建（无 bookSourceId）时用返回的 id 追加到本地列表', async () => {
    getBookSourceListMock.mockResolvedValue([fullSource({ bookSourceId: 1 })])
    saveBookSourceMock.mockResolvedValue({ saved: true, bookSourceId: 9 })
    const store = useSourceStore()
    await store.fetchSources()

    const created = fullSource({ bookSourceName: '新源' })
    delete created.bookSourceId
    await store.saveSource(created)

    expect(store.sources.map((s) => s.bookSourceId)).toEqual([1, 9])
    expect(store.sources[1].bookSourceName).toBe('新源')
  })

  it('deleteSource 调后端后局部移除并重写快照（摘要列表足够，不依赖全量）', async () => {
    getBookSourceListMock.mockResolvedValue([
      fullSource({ bookSourceId: 1 }),
      fullSource({ bookSourceId: 2 }),
    ])
    deleteBookSourceMock.mockResolvedValue({ deleted: true })
    const store = useSourceStore()
    await store.fetchSources()

    await store.deleteSource(1)

    expect(deleteBookSourceMock).toHaveBeenCalledWith(1)
    expect(store.sources.map((s) => s.bookSourceId)).toEqual([2])
    expect(readSnapshotRaw().sources.map((s) => s.bookSourceId)).toEqual([2])
  })

  it('deleteSources 批量删除后局部移除', async () => {
    getBookSourceListMock.mockResolvedValue([
      fullSource({ bookSourceId: 1 }),
      fullSource({ bookSourceId: 2 }),
      fullSource({ bookSourceId: 3 }),
    ])
    deleteBookSourcesMock.mockResolvedValue({ deleted: true })
    const store = useSourceStore()
    await store.fetchSources()

    await store.deleteSources([1, 3])

    expect(deleteBookSourcesMock).toHaveBeenCalledWith([{ bookSourceId: 1 }, { bookSourceId: 3 }])
    expect(store.sources.map((s) => s.bookSourceId)).toEqual([2])
  })

  it('saveSources 批量导入后强制重拉摘要（导入源由后端分配 id）', async () => {
    getBookSourceListMock.mockResolvedValueOnce([fullSource({ bookSourceId: 1 })])
    saveBookSourcesMock.mockResolvedValue({ saved: true, count: 2 })
    const store = useSourceStore()
    await store.fetchSources()

    getBookSourceListMock.mockResolvedValueOnce([
      fullSource({ bookSourceId: 1 }),
      fullSource({ bookSourceId: 2, bookSourceName: '导入A' }),
      fullSource({ bookSourceId: 3, bookSourceName: '导入B' }),
    ])
    await store.saveSources([fullSource({ bookSourceName: '导入A' }), fullSource({ bookSourceName: '导入B' })])

    expect(saveBookSourcesMock).toHaveBeenCalledTimes(1)
    expect(getBookSourceListMock).toHaveBeenCalledTimes(2)
    expect(store.sources.map((s) => s.bookSourceId)).toEqual([1, 2, 3])
    expect(store.partial).toBe(false)
  })

  it('saveSources 保存成功但重拉失败：整体仍 resolve，置 partial 待补全，本地列表保持', async () => {
    getBookSourceListMock.mockResolvedValueOnce([fullSource({ bookSourceId: 1 })])
    saveBookSourcesMock.mockResolvedValue({ saved: true, count: 1 })
    const store = useSourceStore()
    await store.fetchSources()
    expect(store.partial).toBe(false)

    // 导入已落库，但随后的强制重拉失败（断网等）
    getBookSourceListMock.mockRejectedValueOnce(new Error('network down'))
    const result = await store.saveSources([fullSource({ bookSourceName: '导入A' })])

    // 不报假失败：按保存结果 resolve
    expect(result).toEqual({ saved: true, count: 1 })
    expect(saveBookSourcesMock).toHaveBeenCalledTimes(1)
    expect(getBookSourceListMock).toHaveBeenCalledTimes(2)
    // partial 标记让下一次 ensureList 补全；本地列表暂缺新导入项但不被清空
    expect(store.partial).toBe(true)
    expect(store.sources.map((s) => s.bookSourceId)).toEqual([1])
  })

  it('saveSources 重拉失败后，下一次 ensureList 能补全并清除 partial', async () => {
    getBookSourceListMock.mockResolvedValueOnce([fullSource({ bookSourceId: 1 })])
    saveBookSourcesMock.mockResolvedValue({ saved: true, count: 1 })
    const store = useSourceStore()
    await store.fetchSources()

    getBookSourceListMock.mockRejectedValueOnce(new Error('network down'))
    await store.saveSources([fullSource({ bookSourceName: '导入A' })])
    expect(store.partial).toBe(true)

    getBookSourceListMock.mockResolvedValueOnce([
      fullSource({ bookSourceId: 1 }),
      fullSource({ bookSourceId: 2, bookSourceName: '导入A' }),
    ])
    await store.ensureList()

    expect(store.partial).toBe(false)
    expect(store.sources.map((s) => s.bookSourceId)).toEqual([1, 2])
  })

  it('saveSources 保存本身失败：reject 且不触发重拉', async () => {
    getBookSourceListMock.mockResolvedValueOnce([fullSource({ bookSourceId: 1 })])
    saveBookSourcesMock.mockRejectedValue(new Error('save failed'))
    const store = useSourceStore()
    await store.fetchSources()

    await expect(
      store.saveSources([fullSource({ bookSourceName: '导入A' })])
    ).rejects.toThrow('save failed')

    expect(getBookSourceListMock).toHaveBeenCalledTimes(1) // 仅首次加载，未重拉
    expect(store.partial).toBe(false)
  })

  it('deleteInvalidSources 按「失效」分组局部移除', async () => {
    getBookSourceListMock.mockResolvedValue([
      fullSource({ bookSourceId: 1, bookSourceGroup: '默认,失效' }),
      fullSource({ bookSourceId: 2, bookSourceGroup: '默认' }),
    ])
    deleteInvalidBookSourcesMock.mockResolvedValue({ deleted: 1 })
    const store = useSourceStore()
    await store.fetchSources()

    const result = await store.deleteInvalidSources()

    expect(deleteInvalidBookSourcesMock).toHaveBeenCalledTimes(1)
    expect(result.deleted).toBe(1)
    expect(store.sources.map((s) => s.bookSourceId)).toEqual([2])
  })

  it('ensureList 在快照路径等后台摘要落地（partial 转 false）', async () => {
    seedSnapshot([fullSource({ bookSourceName: '快照源' })])
    let resolveList!: (list: BookSourceSummary[]) => void
    getBookSourceListMock.mockImplementation(
      () => new Promise<BookSourceSummary[]>((resolve) => (resolveList = resolve))
    )
    const store = useSourceStore()
    await store.fetchSources()
    expect(store.partial).toBe(true)

    const ensurePromise = store.ensureList()
    resolveList([fullSource({ bookSourceId: 1, bookSourceName: '摘要源' })])
    await ensurePromise

    expect(store.partial).toBe(false)
    expect(store.sources.map((s) => s.bookSourceName)).toEqual(['摘要源'])
  })

  it('ensureList 在后台摘要失败且仅有快照时强制补拉一次，仍失败才抛错', async () => {
    seedSnapshot([fullSource({ bookSourceName: '快照源' })])
    getBookSourceListMock.mockRejectedValue(new Error('network down'))
    const store = useSourceStore()
    await store.fetchSources()
    await vi.waitFor(() => expect(store.loading).toBe(false))
    expect(store.partial).toBe(true)

    await expect(store.ensureList()).rejects.toThrow('书源列表尚未加载完整')
    // 首次 + 补拉，共两次
    expect(getBookSourceListMock).toHaveBeenCalledTimes(2)
  })
})
