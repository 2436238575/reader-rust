import http from './http'
import { API_BASE } from '../utils/appBase'
import { appendAuthQueryParams } from '../utils/secureAccess'

/**
 * SSE-based book caching. Returns an EventSource.
 */
export function cacheBookSSE(params: {
  bookUrl: string
  tocUrl?: string
  count?: number
  refresh?: number
  concurrentCount?: number
}) {
  const query = new URLSearchParams()
  query.set('bookUrl', params.bookUrl)
  if (params.tocUrl) query.set('tocUrl', params.tocUrl)
  if (params.count) query.set('count', String(params.count))
  if (params.refresh) query.set('refresh', String(params.refresh))
  if (params.concurrentCount) query.set('concurrentCount', String(params.concurrentCount))

  appendAuthQueryParams(query)

  return new EventSource(`${API_BASE}/cacheBookSSE?${query.toString()}`)
}

/**
 * Delete all content cache for a book
 */
export function deleteBookCache(bookUrl: string) {
  return http.post('/deleteBookCache', { bookUrl }).then((r) => r.data)
}

/** 服务端缓存分层，与后端 `CacheKind`（camelCase）一一对应 */
export type ServerCacheKind = 'content' | 'cover' | 'chapterList' | 'searchResults' | 'review'

export interface CacheUsage {
  files: number
  bytes: number
}

export type CacheStats = Record<ServerCacheKind, CacheUsage>

/** 各层被删除的文件数，与后端 `CachePurgeResult` 对应 */
export type CachePurgeCounts = Record<ServerCacheKind | 'invalidSources', number>

/** purgeCache 的响应 data：{ purged: 各层删除数 } */
export interface CachePurgeResult {
  purged: CachePurgeCounts
}

/** 各层缓存的当前占用（当前用户） */
export function getCacheStats() {
  return http.get<CacheStats>('/cacheStats').then((r) => r.data)
}

/** 清理当前用户某一层缓存 */
export function purgeCacheByKind(kind: ServerCacheKind) {
  return http
    .post<CachePurgeResult>('/purgeCache', { scope: 'kind', kind })
    .then((r) => r.data)
}

/** 清理当前用户的全部缓存 */
export function purgeAllUserCache() {
  return http.post<CachePurgeResult>('/purgeCache', { scope: 'user' }).then((r) => r.data)
}
