import { createAuthedEventSource } from '../utils/authedEventSource'
import { API_V1_BASE } from '../utils/appBase'
import http from './http'
import type { components } from './generated'

/** 缓存内全文检索的单章命中（生成类型，对应 GET /v1/books/{id}/cache-search） */
export type CachedSearchHit = components['schemas']['CachedSearchHitItem']
export type CachedSearchOutcome = components['schemas']['CachedSearchResponse']

/**
 * 缓存内全文检索：只扫已缓存章节（可再生数据），不触发任何上游请求；
 * 本地书不在检索范围。
 */
export function searchCachedContent(bookId: string, query: string, maxResults?: number) {
  return http
    .get<CachedSearchOutcome>(
      `/books/${encodeURIComponent(bookId)}/cache-search`,
      {
        baseURL: API_V1_BASE,
        params: { q: query, limit: maxResults },
      }
    )
    .then((r) => r.data)
}

/**
 * SSE 换源候选搜索（v1）。Caller is responsible for closing the connection.
 */
export function getAvailableBookSourceSSE(params: {
  bookId: string
  refresh?: boolean
  lastIndex?: number
  concurrentCount?: number
}) {
  return createAuthedEventSource(
    `/books/${encodeURIComponent(params.bookId)}/source-candidates/events`,
    {
      refresh: params.refresh,
      lastIndex: params.lastIndex ?? -1,
      concurrentCount: params.concurrentCount ?? 8,
    },
    API_V1_BASE
  )
}
