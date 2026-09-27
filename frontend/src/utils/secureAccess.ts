import { API_BASE } from './appBase'

export const ACCESS_TOKEN_STORAGE_KEY = 'accessToken'

type StorageLike = Pick<Storage, 'getItem'>

/**
 * 读取本地保存的 JWT。
 *
 * 令牌由服务端签发、前端只负责原样回传；其内部格式（`sub`/`ns`/`exp` 等）
 * 完全由后端定义，前端不做任何解析。
 */
export function readAccessToken(storage: StorageLike = localStorage): string | undefined {
  return storage.getItem(ACCESS_TOKEN_STORAGE_KEY)?.trim() || undefined
}

/**
 * 把令牌放进查询参数。
 *
 * SSE 由浏览器 `EventSource` 发起、`<img>` 也带不了请求头，这些请求只能
 * 用查询参数携带身份，因此这条路径不是可选的兼容分支。
 */
export function appendAuthQueryParams(
  params: URLSearchParams,
  storage: StorageLike = localStorage
) {
  const token = readAccessToken(storage)
  if (token) {
    params.set('accessToken', token)
  }
}

/**
 * 构造带鉴权查询串的 EventSource（SSE 无法设置请求头，只能走查询串）。
 * `undefined` 的参数自动跳过。调用方负责关闭返回的连接。
 */
export function createAuthedEventSource(
  path: string,
  params: Record<string, string | number | undefined>,
): EventSource {
  const query = new URLSearchParams()
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) {
      query.set(key, String(value))
    }
  }
  appendAuthQueryParams(query)
  return new EventSource(`${API_BASE}${path}?${query.toString()}`)
}
