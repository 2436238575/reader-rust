import http from './http'
import { API_V1_BASE } from '../utils/appBase'

/** 可清理的服务端缓存层，与后端 `CacheKind`（camelCase）一一对应 */
export type ServerCacheKind = 'cover' | 'chapterList' | 'searchResults' | 'review'

export interface CacheUsage {
  files: number
  bytes: number
}

/** 正文历史只读统计：正文池条数、章节数、版本数和落库字节数 */
export interface ContentHistoryStats extends CacheUsage {
  chapters: number
  versions: number
}

export type CacheStats = Record<ServerCacheKind, CacheUsage> & {
  content: ContentHistoryStats
}

/** 各层被删除的文件数，与后端 `CachePurgeResult` 对应（正文历史恒为 0） */
export type CachePurgeCounts = Record<ServerCacheKind | 'content' | 'invalidSources', number>

/** purgeCache 的响应 data：{ purged: 各层删除数 } */
export interface CachePurgeResult {
  purged: CachePurgeCounts
}

/** 各层缓存的当前占用（当前用户视角） */
export function getCacheStats() {
  return http
    .get<CacheStats>('/cache/stats', { baseURL: API_V1_BASE })
    .then((r) => r.data)
}

/** 清理当前用户某一层缓存 */
export function purgeCacheByKind(kind: ServerCacheKind) {
  return http
    .delete<CachePurgeResult>('/cache', {
      baseURL: API_V1_BASE,
      params: { kind },
    })
    .then((r) => r.data)
}

/** 清理当前用户的全部缓存 */
export function purgeAllUserCache() {
  return http
    .delete<CachePurgeResult>('/cache', { baseURL: API_V1_BASE })
    .then((r) => r.data)
}
