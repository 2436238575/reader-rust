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

const http = axios.create({
  baseURL: API_BASE,
  timeout: 120000,
  // 会话凭据在 HttpOnly Cookie 里，浏览器自动携带；跨域 CORS 部署也要发凭据
  withCredentials: true,
  headers: { 'Content-Type': 'application/json' },
})

/** 登录与刷新本身不参与 401 重试，避免递归。 */
function isAuthFreePath(url: string | undefined): boolean {
  if (!url) return false
  const path = url.split('?')[0]
  return path === '/login' || path === '/refreshToken' || path === '/logout'
}

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
    const data = error.response?.data as Partial<ApiResponse> | undefined
    const status = error.response?.status
    const config = error.config as RetriableConfig | undefined

    // 未登录/会话失效统一为 HTTP 401。本地有登录提示时先静默刷新（凭
    // Cookie 换新）并重放一次；换不到才拉起登录框。并发 401 由
    // authSession 的单飞逻辑合并。
    if (status === 401) {
      const canRetry =
        !!config && !config._authRetried && !isAuthFreePath(config.url) && hasAuthHint()
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

    // errorMsg 是用户可读的文案，直接作为错误信息抛出
    if (data && typeof data === 'object') {
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
