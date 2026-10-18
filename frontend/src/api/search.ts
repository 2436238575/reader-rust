import { createAuthedEventSource } from '../utils/secureAccess'

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
