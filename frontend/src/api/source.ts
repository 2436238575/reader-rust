import http from './http'
import type { BookSource, BookSourceSummary, BookSourceTestResponse } from '../types'
import { MAX_SOURCE_TEST_BATCH_SIZE } from '../utils/sourceTesting'
import { API_V1_BASE } from '../utils/appBase'
import type { components } from './generated'

/** v1 生成的具名 schema（契约 §3.1：手写接口与生成物重复会被打回） */
type ApiSourceSummary = components['schemas']['SourceSummary']
type ApiSourcePage = components['schemas']['SourcePage']

/** 全量列表（含规则 JSON，可达 MB 级）：只给导出/WebDAV 备份等真正需要全量的低频场景 */
export function getBookSources() {
  return http.get<BookSource[]>('/getBookSources').then((r) => r.data)
}

/** `src_<n>` → 数字 id（过渡期适配：现有组件仍消费数字 id，全面切 v1 类型时删除） */
export function parseSourceId(v1Id: string): number {
  return Number(v1Id.replace(/^src_/, ''))
}

/** 数字 id → `src_<n>`（写接口的路径/请求体身份） */
export function buildSourceId(bookSourceId: number): string {
  return `src_${bookSourceId}`
}

function toLegacySummary(item: ApiSourceSummary): BookSourceSummary {
  const updatedMs = item.updatedAt ? Date.parse(item.updatedAt) : Number.NaN
  return {
    bookSourceId: parseSourceId(item.id),
    bookSourceName: item.name,
    bookSourceUrl: item.url,
    bookSourceGroup: item.group ?? undefined,
    bookSourceType: item.type ?? undefined,
    enabled: item.enabled,
    enabledExplore: item.enabledExplore,
    enabledCookieJar: item.enabledCookieJar,
    lastUpdateTime: Number.isFinite(updatedMs) ? updatedMs : undefined,
    respondTime: item.respondTime ?? undefined,
    customOrder: item.customOrder ?? undefined,
    weight: item.weight ?? undefined,
    exploreUrl: item.exploreUrl ?? undefined,
  } satisfies BookSourceSummary
}

/**
 * 摘要列表：走 v1 游标分页接口拉全量后适配（书源数量在几百级，200/页最多几个来回）。
 * 摘要不含规则 JSON；编辑器等按 id 取全量的场景继续走 /reader3（sources 写批次迁移）。
 */
export async function getBookSourceList(): Promise<BookSourceSummary[]> {
  const out: BookSourceSummary[] = []
  let cursor: string | null = null
  do {
    const params: { limit: number; cursor?: string } = { limit: 200 }
    if (cursor) params.cursor = cursor
    const page: ApiSourcePage = await http
      .get<ApiSourcePage>('/sources', { baseURL: API_V1_BASE, params })
      .then((r) => r.data)
    out.push(...page.items.map(toLegacySummary))
    cursor = page.nextCursor ?? null
  } while (cursor)
  return out
}

/** 单条全量：编辑器/启停翻转等「按整条 upsert 保存」的场景按需取。
 *  v1 详情是源材料透传（含数字 bookSourceId 字段），与旧形状一致，无需映射 */
export function getBookSource(bookSourceId: number) {
  return http
    .get<BookSource>(`/sources/${buildSourceId(bookSourceId)}`, { baseURL: API_V1_BASE })
    .then((r) => r.data)
}

export function loginBookSource(bookSourceId: number) {
  return http
    .post<{
      success: boolean
      status: number
      url: string
      checkResult?: string | null
      bodyPreview?: string
      bodyHtml?: string
    }>('/loginBookSource', { bookSourceId })
    .then((r) => r.data)
}

/** 换取书源代理限定令牌：绑定单个书源、30 分钟有效，只能访问代理路径；主 JWT 不进 URL。 */
export function mintBookSourceProxyToken(params: { bookSourceId: number }) {
  return http
    .post<{ token: string; expiresIn: number }>('/mintBookSourceProxyToken', params)
    .then((r) => r.data)
}

/** 单条全量 upsert：按自带 id 更新，否则按 URL 保号归并（201 新建 / 200 替换） */
export function saveBookSource(source: BookSource) {
  return http
    .post<{ id: string }>('/sources', source, { baseURL: API_V1_BASE })
    .then((r) => ({ saved: true, bookSourceId: parseSourceId(r.data.id) }))
}

/** 批量导入/订阅/恢复：URL 去重优先（保本地 id），单事务提交 */
export function saveBookSources(sources: BookSource[]) {
  return http
    .post<{ count: number }>('/sources:batchUpsert', { sources }, { baseURL: API_V1_BASE })
    .then((r) => ({ saved: true, count: r.data.count }))
}

/** 单条删除（幂等 204） */
export function deleteBookSource(bookSourceId: number) {
  return http
    .delete(`/sources/${buildSourceId(bookSourceId)}`, { baseURL: API_V1_BASE })
    .then(() => ({ deleted: true }))
}

/** 批量删除（幂等；不存在的 id 忽略） */
export function deleteBookSources(sources: { bookSourceId: number }[]) {
  const ids = sources.map((source) => buildSourceId(source.bookSourceId))
  return http
    .post<{ deleted: number }>('/sources:batchDelete', { ids }, { baseURL: API_V1_BASE })
    .then((r) => ({ deleted: r.data.deleted > 0 }))
}

/** 清空全部书源：confirm 体是毁灭性操作的门槛 */
export function deleteAllBookSources() {
  return http
    .delete<{ deleted: number }>(
      '/sources',
      { baseURL: API_V1_BASE, data: { confirm: 'DELETE_ALL' } },
    )
    .then((r) => ({ deleted: r.data.deleted > 0 }))
}

export function testBookSources(params: {
  bookSourceIds?: number[]
  keyword?: string
  markInvalid?: boolean
  concurrent?: number
}) {
  if ((params.bookSourceIds?.length || 0) > MAX_SOURCE_TEST_BATCH_SIZE) {
    throw new Error(`单次最多测试 ${MAX_SOURCE_TEST_BATCH_SIZE} 个书源`)
  }
  return http.post<BookSourceTestResponse>('/testBookSources', params).then((r) => r.data)
}

/** 删除失效书源（分组含「失效」标记），与后端同口径 */
export function deleteInvalidBookSources() {
  return http
    .post<{ deleted: number }>('/sources:deleteInvalid', {}, { baseURL: API_V1_BASE })
    .then((r) => r.data)
}

export function readRemoteSourceFile(url: string) {
  return http.post<BookSource[]>('/readRemoteSourceFile', { url }).then((r) => r.data)
}

export function readSourceFile(file: File) {
  const formData = new FormData()
  formData.append('file', file)
  return http
    .post<BookSource[]>('/readSourceFile', formData, {
      headers: {
        'Content-Type': 'multipart/form-data',
      },
    })
    .then((r) => r.data)
}
