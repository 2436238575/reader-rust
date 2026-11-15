import http from './http'
import { API_V1_BASE } from '../utils/appBase'
import type { components } from './generated'

export type ReadEventsBatchResult = components['schemas']['ReadEventsBatchResult']
export type ReadingStats = components['schemas']['ReadingStats']

export interface ReadEventPayload {
  bookId: string
  chapterIndex: number
  seconds: number
}

/** 上报阅读事件：单批 ≤100；服务端按 (书, 章, 日) 取累计秒数 MAX。 */
export function reportReadEvents(
  events: ReadEventPayload[],
  dayKey?: number | null
) {
  return http
    .post<ReadEventsBatchResult>(
      '/me/read-events:batch',
      {
        events,
        ...(dayKey != null ? { dayKey } : {}),
      },
      { baseURL: API_V1_BASE }
    )
    .then((r) => r.data)
}

/** 当前用户的阅读统计（服务端从明细派生）。 */
export function getReadingStats() {
  return http
    .get<ReadingStats>('/me/stats', { baseURL: API_V1_BASE })
    .then((r) => r.data)
}
