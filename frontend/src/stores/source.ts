import { defineStore } from 'pinia'
import { ref, shallowRef } from 'vue'
import {
  getBookSourceList,
  saveBookSource,
  saveBookSources,
  deleteBookSource,
  deleteBookSources,
  deleteInvalidBookSources,
} from '../api/source'
import type { BookSource, BookSourceSummary } from '../types'
import { splitBookSourceGroups } from '../utils/sourceSelection'

/**
 * 书源冷启动快照：首屏先用本地快照渲染，后台再拉摘要列表替换。
 * 快照只存展示所需的非敏感字段——header/loginUrl/jsLib/各类规则字段一律不落盘。
 */
export const SOURCE_SNAPSHOT_KEY = 'reader-source-snapshot-v1'
const SOURCE_SNAPSHOT_TTL_MS = 24 * 60 * 60 * 1000
const SOURCE_SNAPSHOT_MAX_SOURCES = 1000

/** 快照白名单字段（与 SourceSummary 一一对应，导出供测试对比） */
export const SOURCE_SNAPSHOT_FIELDS = [
  'bookSourceId',
  'bookSourceName',
  'bookSourceUrl',
  'bookSourceGroup',
  'bookSourceType',
  'enabled',
  'enabledExplore',
  'lastUpdateTime',
  'respondTime',
  'customOrder',
  'weight',
] as const

export type SourceSummary = Pick<BookSource, (typeof SOURCE_SNAPSHOT_FIELDS)[number]>

type SourceSnapshot = {
  savedAt: number
  sources: SourceSummary[]
}

/** 全量/摘要书源 → 快照条目：严格按白名单挑字段，敏感字段（header/loginUrl/jsLib/规则）不落盘 */
function toSummary(source: BookSource | BookSourceSummary): SourceSummary {
  return {
    bookSourceId: source.bookSourceId,
    bookSourceName: source.bookSourceName,
    bookSourceUrl: source.bookSourceUrl,
    bookSourceGroup: source.bookSourceGroup,
    bookSourceType: source.bookSourceType,
    enabled: source.enabled,
    enabledExplore: source.enabledExplore,
    lastUpdateTime: source.lastUpdateTime,
    respondTime: source.respondTime,
    customOrder: source.customOrder,
    weight: source.weight,
  }
}

/** 全量书源 → 本地列表摘要条目（含 exploreUrl，发现页解析分类用；规则字段不带） */
function toListSummary(source: BookSource): BookSourceSummary {
  return {
    bookSourceId: source.bookSourceId,
    bookSourceName: source.bookSourceName,
    bookSourceUrl: source.bookSourceUrl,
    bookSourceGroup: source.bookSourceGroup,
    bookSourceType: source.bookSourceType,
    enabled: source.enabled,
    enabledExplore: source.enabledExplore,
    enabledCookieJar: source.enabledCookieJar,
    lastUpdateTime: source.lastUpdateTime,
    respondTime: source.respondTime,
    customOrder: source.customOrder,
    weight: source.weight,
    exploreUrl: source.exploreUrl,
  }
}

/** 读快照；损坏/过期/超限一律删除并返回 null（按普通摘要加载） */
function readSnapshot(): SourceSummary[] | null {
  let raw: string | null = null
  try {
    raw = localStorage.getItem(SOURCE_SNAPSHOT_KEY)
  } catch {
    return null
  }
  if (!raw) return null
  const discard = () => {
    try {
      localStorage.removeItem(SOURCE_SNAPSHOT_KEY)
    } catch {
      /* localStorage 不可用时无可清理 */
    }
  }
  try {
    const parsed = JSON.parse(raw) as SourceSnapshot
    const fresh =
      typeof parsed?.savedAt === 'number' && Date.now() - parsed.savedAt < SOURCE_SNAPSHOT_TTL_MS
    if (!fresh || !Array.isArray(parsed.sources) || parsed.sources.length > SOURCE_SNAPSHOT_MAX_SOURCES) {
      discard()
      return null
    }
    // 逐条过白名单消毒：即使快照被篡改，内存中的占位对象也不带敏感字段
    const summaries = parsed.sources.map((source) => toSummary(source as BookSource))
    if (summaries.some((s) => typeof s.bookSourceName !== 'string' || typeof s.bookSourceUrl !== 'string')) {
      discard()
      return null
    }
    return summaries
  } catch {
    discard()
    return null
  }
}

function writeSnapshot(list: readonly BookSourceSummary[]) {
  try {
    const snapshot: SourceSnapshot = {
      savedAt: Date.now(),
      sources: list.slice(0, SOURCE_SNAPSHOT_MAX_SOURCES).map(toSummary),
    }
    localStorage.setItem(SOURCE_SNAPSHOT_KEY, JSON.stringify(snapshot))
  } catch {
    // localStorage 满/不可用：快照只是加速首屏，写失败不影响功能
  }
}

export const useSourceStore = defineStore('source', () => {
  // 本地列表只存摘要（规则 JSON 可达 MB 级，按需单条拉取）：浅响应式避免深代理几百个源
  const sources = shallowRef<BookSourceSummary[]>([])
  const loading = ref(false)
  /** true 表示当前 sources 只是本地快照（缺 exploreUrl），摘要列表尚未就位 */
  const partial = ref(false)
  let loadedOnce = false
  let listLoadTask: Promise<void> | null = null

  function startListLoad(): Promise<void> {
    if (listLoadTask) return listLoadTask
    loading.value = true
    listLoadTask = getBookSourceList()
      .then((list) => {
        sources.value = list
        partial.value = false
        loadedOnce = true
        writeSnapshot(list)
      })
      .finally(() => {
        loading.value = false
        listLoadTask = null
      })
    return listLoadTask
  }

  async function fetchSources(options: { force?: boolean } = {}) {
    // 首次加载且有新鲜快照：先快照渲染并立即返回，后台拉摘要（失败保留快照）
    if (!options.force && !loadedOnce) {
      const snapshot = readSnapshot()
      if (snapshot) {
        sources.value = snapshot
        partial.value = true
        loadedOnce = true
        startListLoad().catch(() => undefined)
        return
      }
    }
    await startListLoad()
  }

  /**
   * 摘要列表就位保障：快照占位（缺 exploreUrl）或后台加载失败时补拉；
   * 仍拿不到则抛错，调用方（发现页等）据此提示而不是拿残缺列表继续。
   * 编辑器/启停翻转需要单条全量的场景不走这里：按 id 调 getBookSource。
   */
  async function ensureList(): Promise<void> {
    if (!loadedOnce) await fetchSources()
    // 快照路径下 fetchSources 立即返回，后台摘要在途：等它落地（失败不外抛，走下面的补拉）
    if (listLoadTask) await listLoadTask.catch(() => undefined)
    if (partial.value) await fetchSources({ force: true }).catch(() => undefined)
    if (partial.value) {
      throw new Error('书源列表尚未加载完整，请检查网络后重试')
    }
  }

  function upsertLocal(summary: BookSourceSummary) {
    const id = summary.bookSourceId
    const index = id == null ? -1 : sources.value.findIndex((s) => s.bookSourceId === id)
    const next = [...sources.value]
    if (index >= 0) {
      next[index] = summary
    } else {
      next.push(summary)
    }
    sources.value = next
    writeSnapshot(next)
  }

  function removeLocal(predicate: (summary: BookSourceSummary) => boolean) {
    const next = sources.value.filter((summary) => !predicate(summary))
    sources.value = next
    writeSnapshot(next)
  }

  /**
   * 单书保存：source 必须是单条全量对象（getBookSource 取回或编辑器解析结果），
   * 后端按整行 upsert；成功后按摘要投影局部更新本地列表，不整表重拉。
   */
  async function saveSource(source: BookSource) {
    const saved = await saveBookSource(source)
    upsertLocal(toListSummary({ ...source, bookSourceId: saved.bookSourceId }))
    return saved
  }

  async function deleteSource(bookSourceId: number) {
    const result = await deleteBookSource(bookSourceId)
    removeLocal((summary) => summary.bookSourceId === bookSourceId)
    return result
  }

  async function deleteSources(bookSourceIds: number[]) {
    const result = await deleteBookSources(bookSourceIds.map((bookSourceId) => ({ bookSourceId })))
    const idSet = new Set(bookSourceIds)
    removeLocal((summary) => summary.bookSourceId != null && idSet.has(summary.bookSourceId))
    return result
  }

  /**
   * 批量导入：导入源由后端分配/归并 id，成功后强制重拉摘要对齐。
   * 重拉是 best-effort：失败时置 partial（本地列表暂缺新导入项），
   * 由下一次 ensureList 补全，不把已成功落库的导入报成失败。
   */
  async function saveSources(list: BookSource[]) {
    const result = await saveBookSources(list)
    try {
      await fetchSources({ force: true })
    } catch {
      partial.value = true
    }
    return result
  }

  /** 删除失效书源：与后端同口径（分组含「失效」），成功后局部移除 */
  async function deleteInvalidSources() {
    const result = await deleteInvalidBookSources()
    removeLocal((summary) => splitBookSourceGroups(summary.bookSourceGroup).includes('失效'))
    return result
  }

  return {
    sources,
    loading,
    partial,
    fetchSources,
    ensureList,
    saveSource,
    saveSources,
    deleteSource,
    deleteSources,
    deleteInvalidSources,
  }
})
