import http from './http'
import type { UserInfo } from '../types'

export function login(username: string, password: string) {
  return http.post<UserInfo>('/login', { username, password }).then((r) => r.data)
}

export function logout() {
  return http.post('/logout').catch(() => {
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

/// 改密码会作废其他设备上的令牌，并返回当前设备换发的新令牌
export function changePassword(oldPassword: string, newPassword: string) {
  return http
    .post<UserInfo>('/changePassword', { oldPassword, newPassword })
    .then((r) => r.data)
}
