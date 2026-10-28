import http from './http'
import type { BookSource, BookSourceSummary, BookSourceTestResponse } from '../types'
import { MAX_SOURCE_TEST_BATCH_SIZE } from '../utils/sourceTesting'

/** 全量列表（含规则 JSON，可达 MB 级）：只给导出/WebDAV 备份等真正需要全量的低频场景 */
export function getBookSources() {
  return http.get<BookSource[]>('/getBookSources').then((r) => r.data)
}

/** 摘要列表：展示字段 + exploreUrl，无规则 JSON；书源管理页的日常加载走这条 */
export function getBookSourceList() {
  return http.get<BookSourceSummary[]>('/getBookSourceList').then((r) => r.data)
}

/** 单条全量：编辑器/启停翻转等「按整条 upsert 保存」的场景按需取 */
export function getBookSource(bookSourceId: number) {
  return http
    .get<BookSource>('/getBookSource', { params: { bookSourceId } })
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

export function saveBookSource(source: BookSource) {
  return http
    .post<{ saved: boolean; bookSourceId: number }>('/saveBookSource', source)
    .then((r) => r.data)
}

export function saveBookSources(sources: BookSource[]) {
  return http
    .post<{ saved: boolean; count: number }>('/saveBookSources', sources)
    .then((r) => r.data)
}

export function deleteBookSource(bookSourceId: number) {
  return http.post<{ deleted: boolean }>('/deleteBookSource', { bookSourceId }).then((r) => r.data)
}

export function deleteBookSources(sources: { bookSourceId: number }[]) {
  return http.post<{ deleted: boolean }>('/deleteBookSources', sources).then((r) => r.data)
}

export function deleteAllBookSources() {
  return http.post<{ deleted: boolean }>('/deleteAllBookSources').then((r) => r.data)
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

export function deleteInvalidBookSources() {
  return http.post<{ deleted: number }>('/deleteInvalidBookSources').then((r) => r.data)
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
