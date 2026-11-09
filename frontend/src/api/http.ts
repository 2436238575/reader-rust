import axios, { type InternalAxiosRequestConfig } from 'axios'
import type { ApiResponse } from '../types'
import { hasAuthHint } from '../utils/secureAccess'
import { API_BASE } from '../utils/appBase'
import { notifyNeedLogin } from '../utils/authEvents'
import {
  RefreshNetworkError,
  SessionExpiredError,
  refreshAccessToken,
} from '../utils/authSession'

type RetriableConfig = InternalAxiosRequestConfig & { _authRetried?: boolean }

/**
 * 大文件上传的超时，与后端 `INBOUND_UPLOAD_TIMEOUT_SECS`（默认 900 秒）对齐。
 *
 * 默认的 120 秒对几十 MB 的整本 TXT 不够：慢链路或经反向代理时请求会被
 * 前端主动掐断，看起来就像上传到一半被截断。
 */
export const UPLOAD_TIMEOUT_MS = 900_000

const http = axios.create({
  baseURL: API_BASE,
  timeout: 120000,
  // 会话凭据在 HttpOnly Cookie 里，浏览器自动携带；跨域 CORS 部署也要发凭据
  withCredentials: true,
  headers: {
    'Content-Type': 'application/json',
    // `/v1` 的非 GET 一律要求该头（CSRF 双保险）；`/reader3` 忽略它，无害
    'X-Requested-With': 'XMLHttpRequest',
  },
})

/** 登录与刷新本身不参与 401 重试，避免递归。 */
function isAuthFreePath(url: string | undefined): boolean {
  if (!url) return false
  const path = url.split('?')[0]
  return (
    path === '/auth/login' || path === '/auth/refresh' || path === '/auth/logout'
  )
}

/** 凭据本身无效的错误码：静默刷新救不回来，直接走登录流程。 */
const DEAD_CREDENTIAL_CODES = new Set([
  'INVALID_CREDENTIALS',
  'AUTH_REFRESH_REPLAYED',
  'SESSION_INVALID',
])

// ─── Response interceptor: unwrap ApiResponse, refresh on 401 ───
// 请求侧不再注入 Authorization：浏览器会自动带上 HttpOnly Cookie。
http.interceptors.response.use(
  (response) => {
    const data = response.data as ApiResponse
    // Some endpoints return raw data (cover, file etc.)
    if (data.isSuccess === undefined) {
      return response
    }
    if (!data.isSuccess) {
      return Promise.reject(new Error(data.errorMsg || '请求失败'))
    }
    // Return unwrapped data
    response.data = data.data
    return response
  },
  async (error) => {
    const data = error.response?.data as
      | (Partial<ApiResponse> & { code?: string; detail?: string })
      | undefined
    const status = error.response?.status
    const config = error.config as RetriableConfig | undefined
    // /v1 的错误是 problem+json：detail 给用户、code 给分支判断
    const problemCode = typeof data?.code === 'string' ? data.code : undefined

    // 413 可能来自反向代理（nginx 默认 client_max_body_size 只有 1m），
    // 此时响应体是 HTML，没有 errorMsg；给一句能直接照着改配置的提示。
    if (status === 413) {
      return Promise.reject(
        withStatus(
          new Error(
            '上传内容过大（HTTP 413）：请检查 nginx/OpenResty 的 client_max_body_size（建议 ≥100m）与上游 WAF 的上传上限'
          ),
          status
        )
      )
    }

    // 未登录/会话失效统一为 HTTP 401。本地有登录提示时先静默刷新（凭
    // Cookie 换新）并重放一次；换不到才拉起登录框。并发 401 由
    // authSession 的单飞逻辑合并。
    if (status === 401) {
      const canRetry =
        !!config &&
        !config._authRetried &&
        !isAuthFreePath(config.url) &&
        !DEAD_CREDENTIAL_CODES.has(problemCode ?? '') &&
        hasAuthHint()
      if (canRetry && config) {
        config._authRetried = true
        try {
          await refreshAccessToken()
          return http.request(config)
        } catch (refreshError) {
          if (refreshError instanceof RefreshNetworkError) {
            return Promise.reject(withStatus(refreshError, status))
          }
          if (refreshError instanceof SessionExpiredError) {
            // refreshAccessToken 已清空提示并通知登录
            return Promise.reject(withStatus(new Error('需要登录'), status))
          }
          return Promise.reject(withStatus(new Error('需要登录'), status))
        }
      }
      notifyNeedLogin()
    }

    // 错误文案：/v1 problem 的 detail 优先，/reader3 回落到 errorMsg；
    // 两者都是用户可读文案，前端只展示不解析
    if (data && typeof data === 'object') {
      if (typeof data.detail === 'string' && data.detail.trim()) {
        const err = withStatus(new Error(data.detail), status) as Error & {
          code?: string
        }
        err.code = problemCode
        return Promise.reject(err)
      }
      if (typeof data.errorMsg === 'string' && data.errorMsg.trim()) {
        return Promise.reject(withStatus(new Error(data.errorMsg), status))
      }
    }
    return Promise.reject(
      withStatus(new Error(error.message || '请求失败'), status)
    )
  }
)

/** 把 HTTP 状态码挂到错误对象上（如 409 冲突检测），不改变既有 message 语义。 */
function withStatus(error: Error, status: number | undefined): Error {
  if (typeof status === 'number') {
    ;(error as Error & { status?: number }).status = status
  }
  return error
}

export default http
