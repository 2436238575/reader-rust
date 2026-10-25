import http from './http'
import type { UserInfo } from '../types'

export function login(username: string, password: string) {
  // 登录成功后凭据由服务端写进 HttpOnly Cookie；响应里的 accessToken 字段
  // 存在也不存，前端只消费用户信息
  return http.post<UserInfo>('/login', { username, password }).then((r) => r.data)
}

export function logout() {
  // 会话吊销靠 reader_refresh Cookie（http 实例统一 withCredentials）；
  // 无论后端是否成功，本地清 hint 都由 store 的 clearUser 负责
  return http.post('/logout', {}).catch(() => {
    // Logout always clears local state
  })
}

export function getUserInfo() {
  return http
    .get<{
      userInfo: UserInfo | null
    }>('/getUserInfo')
    .then((r) => r.data)
}

/// 改密成功后后端会换发 Cookie 并自增撤销版本（其他设备上的会话作废）
export function changePassword(oldPassword: string, newPassword: string) {
  return http.post<UserInfo>('/changePassword', { oldPassword, newPassword }).then((r) => r.data)
}
