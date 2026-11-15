import http from './http'
import type { UserInfo } from '../types'
import { API_V1_BASE } from '../utils/appBase'
import { invalidateSessionForRetry } from '../utils/sessionEpoch'
import type { components } from './generated'

/** v1 生成的具名 schema（契约 §3.1：手写接口与生成物重复会被打回） */
type ApiMe = components['schemas']['Me']

/** 登录响应的令牌对（v1）：refreshToken 只在 HttpOnly Cookie 里。 */
export interface SessionTokens {
  accessToken: string
  accessTokenExpiresAt: string
  username: string
}

export function login(username: string, password: string) {
  // 登录成功后凭据由服务端写进 HttpOnly Cookie；浏览器只消费过期时间与
  // 用户名（accessToken 供 API 客户端，前端不落存储）
  return http
    .post<SessionTokens>('/auth/login', { username, password }, { baseURL: API_V1_BASE })
    .then((r) => r.data)
}

export function logout() {
  // 先让在途请求的 401 失效：登出请求本身不清 Cookie 之前，别的请求返回
  // 401 也不能再触发静默刷新，否则会把刚登出的会话换回来。
  invalidateSessionForRetry()
  // 204 + 吊销 refresh 会话家族；本地清 hint 由 store 的 clearUser 负责
  return http.post('/auth/logout', {}, { baseURL: API_V1_BASE }).catch(() => {
    // Logout always clears local state
  })
}

/**
 * 当前账号资料：走 /v1/me。未登录语义在 api 层收敛为 `userInfo: null`
 * （与旧 /reader3 可选鉴权对齐，store 的登录判定逻辑不变）；401 的静默
 * 刷新与失效处理仍由 http 拦截器 + authSession 完成。
 */
export function getUserInfo() {
  return http
    .get<ApiMe>('/me', { baseURL: API_V1_BASE })
    .then((r) => ({ userInfo: { username: r.data.username } as UserInfo }))
    .catch((error: unknown) => {
      const status = (error as { status?: number }).status
      if (status === 401) return { userInfo: null }
      throw error
    })
}

/// 改密成功（204）：后端吊销其他会话并换发当前会话 Cookie
export function changePassword(currentPassword: string, newPassword: string) {
  return http.post(
    '/auth/password',
    { currentPassword, newPassword },
    { baseURL: API_V1_BASE },
  )
}
