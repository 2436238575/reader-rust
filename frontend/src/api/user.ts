import http from './http'
import type { UserInfo } from '../types'
import { API_V1_BASE } from '../utils/appBase'

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
  // 204 + 吊销 refresh 会话家族；本地清 hint 由 store 的 clearUser 负责
  return http.post('/auth/logout', {}, { baseURL: API_V1_BASE }).catch(() => {
    // Logout always clears local state
  })
}

export function getUserInfo() {
  // 过渡期：资料读取仍在 /reader3（会话 Cookie 两套命名空间共用），
  // me 域切换时随 stats/settings 一起迁移到 /v1/me
  return http
    .get<{
      userInfo: UserInfo | null
    }>('/getUserInfo')
    .then((r) => r.data)
}

/// 改密成功（204）：后端吊销其他会话并换发当前会话 Cookie
export function changePassword(currentPassword: string, newPassword: string) {
  return http.post(
    '/auth/password',
    { currentPassword, newPassword },
    { baseURL: API_V1_BASE },
  )
}
