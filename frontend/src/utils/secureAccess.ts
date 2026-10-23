export const ACCESS_TOKEN_STORAGE_KEY = 'accessToken'
export const REFRESH_TOKEN_STORAGE_KEY = 'refreshToken'
export const ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY = 'accessTokenExpiresAt'

type StorageLike = Pick<Storage, 'getItem'>
type WritableStorageLike = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>

export interface AuthTokens {
  accessToken: string
  refreshToken: string
  /** Access Token 有效秒数（来自登录/刷新响应） */
  accessTokenExpiresIn: number
}

/**
 * 读取本地保存的 Access Token。
 *
 * 令牌由服务端签发、前端只负责原样回传；其内部格式（`sub`/`exp` 等）
 * 完全由后端定义，前端不做任何解析。
 */
export function readAccessToken(storage: StorageLike = localStorage): string | undefined {
  return storage.getItem(ACCESS_TOKEN_STORAGE_KEY)?.trim() || undefined
}

/** 读取 Refresh Token；它只在换取 Access Token 时提交，不进任何 URL。 */
export function readRefreshToken(storage: StorageLike = localStorage): string | undefined {
  return storage.getItem(REFRESH_TOKEN_STORAGE_KEY)?.trim() || undefined
}

/** Access Token 的绝对过期时间（毫秒）；缺失或损坏时返回 undefined。 */
export function readAccessTokenExpiresAt(
  storage: StorageLike = localStorage
): number | undefined {
  const raw = storage.getItem(ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY)
  if (!raw) return undefined
  const value = Number(raw)
  return Number.isFinite(value) && value > 0 ? value : undefined
}

/** 登录 / 刷新 / 改密成功后保存一对新令牌。 */
export function storeAuthTokens(
  tokens: AuthTokens,
  storage: WritableStorageLike = localStorage
) {
  storage.setItem(ACCESS_TOKEN_STORAGE_KEY, tokens.accessToken)
  storage.setItem(REFRESH_TOKEN_STORAGE_KEY, tokens.refreshToken)
  storage.setItem(
    ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY,
    String(Date.now() + tokens.accessTokenExpiresIn * 1000)
  )
}

/** 登出或会话失效时清空全部令牌与过期时间。 */
export function clearAuthTokens(storage: WritableStorageLike = localStorage) {
  storage.removeItem(ACCESS_TOKEN_STORAGE_KEY)
  storage.removeItem(REFRESH_TOKEN_STORAGE_KEY)
  storage.removeItem(ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY)
}

/**
 * 构造带 JWT 的请求头，供裸 fetch 路径使用（keepalive 上报 / blob 下载 /
 * FormData 上传）；axios 实例由 http.ts 的拦截器统一处理，不要走这里。
 * 存储不可读（隐私模式等）时返回空头，与未登录等价。
 */
export function buildAuthHeaders(storage?: StorageLike): Record<string, string> {
  try {
    // localStorage 在无 DOM 环境（Node 测试）里可能根本不存在，引用要放在 try 内
    const token = readAccessToken(storage ?? localStorage)
    return token ? { Authorization: `Bearer ${token}` } : {}
  } catch {
    return {}
  }
}

/**
 * 把 Access Token 放进查询参数。
 *
 * SSE 由浏览器 `EventSource` 发起、`<img>` 也带不了请求头，这些请求只能
 * 用查询参数携带身份，因此这条路径不是可选的兼容分支。
 * Refresh Token 永远不放进 URL。
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
