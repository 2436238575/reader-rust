import { createAuthedEventSource } from '../utils/secureAccess'

/**
 * SSE-based multi-source search. Returns an EventSource.
 * Caller is responsible for closing the connection.
 */
export function searchBookMultiSSE(params: {
  key: string
  bookSourceGroup?: string
  bookSourceUrl?: string
  concurrentCount?: number
  searchSize?: number
}) {
  return createAuthedEventSource('/searchBookMultiSSE', {
    key: params.key,
    bookSourceGroup: params.bookSourceGroup,
    bookSourceUrl: params.bookSourceUrl,
    concurrentCount: params.concurrentCount,
    searchSize: params.searchSize,
  })
}

export function getAvailableBookSourceSSE(params: {
  url?: string
  name?: string
  author?: string
  origin?: string
  refresh?: number
  lastIndex?: number
  concurrentCount?: number
}) {
  return createAuthedEventSource('/getAvailableBookSourceSSE', {
    url: params.url,
    name: params.name,
    author: params.author,
    origin: params.origin,
    refresh: params.refresh,
    lastIndex: params.lastIndex ?? -1,
    concurrentCount: params.concurrentCount ?? 8,
  })
}
