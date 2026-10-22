import { createAuthedEventSource } from '../utils/secureAccess'
import http from './http'

/** 缓存内全文检索的单章命中（后端 searchCachedContent） */
export interface CachedSearchHit {
  chapterIndex: number
  chapterTitle: string
  chapterUrl: string
  snippets: string[]
  /** 命中内容来自编辑版（而非上游缓存） */
  edited: boolean
}

export interface CachedSearchOutcome {
  results: CachedSearchHit[]
  /** 实际参与检索（有正文缓存文件）的章节数 */
  cachedChapters: number
  /** 目录里的可检索章节总数 */
  totalChapters: number
  /** 命中达到上限被截断：后面可能还有结果 */
  truncated: boolean
}

/**
 * 缓存内全文检索：只扫已缓存章节（可再生数据），不触发任何上游请求；
 * 本地书不在检索范围。
 */
export function searchCachedContent(bookId: string, query: string, maxResults?: number) {
  return http
    .post<CachedSearchOutcome>('/searchCachedContent', { bookId, query, maxResults })
    .then((r) => r.data)
}

/**
 * SSE-based multi-source search. Returns an EventSource.
 * Caller is responsible for closing the connection.
 */
export function searchBookMultiSSE(params: {
  key: string
  bookSourceGroup?: string
  bookSourceId?: number
  concurrentCount?: number
  searchSize?: number
}) {
  return createAuthedEventSource('/searchBookMultiSSE', {
    key: params.key,
    bookSourceGroup: params.bookSourceGroup,
    bookSourceId: params.bookSourceId,
    concurrentCount: params.concurrentCount,
    searchSize: params.searchSize,
  })
}

export function getAvailableBookSourceSSE(params: {
  bookId: string
  refresh?: number
  lastIndex?: number
  concurrentCount?: number
}) {
  return createAuthedEventSource('/getAvailableBookSourceSSE', {
    bookId: params.bookId,
    refresh: params.refresh,
    lastIndex: params.lastIndex ?? -1,
    concurrentCount: params.concurrentCount ?? 8,
  })
}
