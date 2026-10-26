import { defineStore } from 'pinia'
import { ref, shallowRef } from 'vue'
import {
  getBookSources,
  saveBookSource,
  saveBookSources,
  deleteBookSource,
  deleteBookSources,
  deleteInvalidBookSources,
} from '../api/source'
import type { BookSource } from '../types'
import { splitBookSourceGroups } from '../utils/sourceSelection'

/**
 * 书源冷启动快照：首屏先用本地快照渲染，后台再拉全量替换。
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

function toSummary(source: BookSource): SourceSummary {
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

/** 读快照；损坏/过期/超限一律删除并返回 null（按普通全量加载） */
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

function writeSnapshot(list: BookSource[]) {
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
  // 书源规则 JSON 可达 MB 级且按整体替换更新：浅响应式避免深代理几百个源
  const sources = shallowRef<BookSource[]>([])
  const loading = ref(false)
  /** true 表示当前 sources 只是本地快照（缺规则字段），全量尚未就位 */
  const partial = ref(false)
  let loadedOnce = false
  let fullLoadTask: Promise<void> | null = null

  function startFullLoad(): Promise<void> {
    if (fullLoadTask) return fullLoadTask
    loading.value = true
    fullLoadTask = getBookSources()
      .then((list) => {
        sources.value = list
        partial.value = false
        loadedOnce = true
        writeSnapshot(list)
      })
      .finally(() => {
        loading.value = false
        fullLoadTask = null
      })
    return fullLoadTask
  }

  async function fetchSources(options: { force?: boolean } = {}) {
    // 首次加载且有新鲜快照：先快照渲染并立即返回，后台拉全量（失败保留快照）
    if (!options.force && !loadedOnce) {
      const snapshot = readSnapshot()
      if (snapshot) {
        sources.value = snapshot
        partial.value = true
        loadedOnce = true
        startFullLoad().catch(() => undefined)
        return
      }
    }
    await startFullLoad()
  }

  /**
   * 编辑/批量写前的门卫：快照对象没有规则字段，直接保存会把规则清空。
   * partial 时等全量就位；等不到（后台失败且强制重拉也失败）则抛错，调用方据此提示。
   */
  async function ensureFullSources() {
    if (!partial.value) {
      if (!loadedOnce) await fetchSources()
      if (!partial.value) return
    }
    await (fullLoadTask ?? fetchSources({ force: true })).catch(() => undefined)
    if (partial.value) {
      throw new Error('书源列表尚未加载完整，请检查网络后重试')
    }
  }

  function upsertLocal(source: BookSource) {
    const index = sources.value.findIndex((s) => s.bookSourceId === source.bookSourceId)
    const next = [...sources.value]
    if (index >= 0) {
      next[index] = source
    } else {
      next.push(source)
    }
    sources.value = next
    writeSnapshot(next)
  }

  function removeLocal(predicate: (source: BookSource) => boolean) {
    const next = sources.value.filter((source) => !predicate(source))
    sources.value = next
    writeSnapshot(next)
  }

  /** 单书保存：先确保拿到全量（避免残缺快照对象覆盖规则），成功后局部 upsert */
  async function saveSource(source: BookSource) {
    await ensureFullSources()
    const saved = await saveBookSource(source)
    upsertLocal({ ...source, bookSourceId: saved.bookSourceId })
    return saved
  }

  async function deleteSource(bookSourceId: number) {
    await ensureFullSources()
    const result = await deleteBookSource(bookSourceId)
    removeLocal((source) => source.bookSourceId === bookSourceId)
    return result
  }

  async function deleteSources(bookSourceIds: number[]) {
    await ensureFullSources()
    const result = await deleteBookSources(bookSourceIds.map((bookSourceId) => ({ bookSourceId })))
    const idSet = new Set(bookSourceIds)
    removeLocal((source) => source.bookSourceId != null && idSet.has(source.bookSourceId))
    return result
  }

  /**
   * 批量导入：导入源由后端分配/归并 id，成功后强制整表重拉对齐。
   * 重拉是 best-effort：失败时置 partial（本地列表暂缺新导入项），
   * 由下一次 ensureFullSources 补全，不把已成功落库的导入报成失败。
   */
  async function saveSources(list: BookSource[]) {
    await ensureFullSources()
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
    await ensureFullSources()
    const result = await deleteInvalidBookSources()
    removeLocal((source) => splitBookSourceGroups(source.bookSourceGroup).includes('失效'))
    return result
  }

  return {
    sources,
    loading,
    partial,
    fetchSources,
    ensureFullSources,
    saveSource,
    saveSources,
    deleteSource,
    deleteSources,
    deleteInvalidSources,
  }
})
