import axios from 'axios'
import type { ApiResponse } from '../types'
import { readAccessToken } from '../utils/secureAccess'

let lastNeedLoginDispatchAt = 0

function dispatchNeedLogin() {
  const now = Date.now()
  if (now - lastNeedLoginDispatchAt < 1500) return
  lastNeedLoginDispatchAt = now
  window.dispatchEvent(new CustomEvent('need-login'))
}

const http = axios.create({
  baseURL: '/reader3',
  timeout: 120000,
  headers: { 'Content-Type': 'application/json' },
})

// ─── Request interceptor: attach the JWT ───
http.interceptors.request.use((config) => {
  const accessToken = readAccessToken(localStorage)
  if (accessToken) {
    config.headers.Authorization = `Bearer ${accessToken}`
  }
  return config
})

// ─── Response interceptor: unwrap ApiResponse ───
http.interceptors.response.use(
  (response) => {
    const data = response.data as ApiResponse
    // Some endpoints return raw data (cover, file etc.)
    if (data.isSuccess === undefined) {
      return response
    }
    if (!data.isSuccess) {
      if (data.errorMsg === 'NEED_LOGIN') {
        dispatchNeedLogin()
      }
      return Promise.reject(new Error(data.errorMsg || '请求失败'))
    }
    // Return unwrapped data
    response.data = data.data
    return response
  },
  (error) => {
    const data = error.response?.data as Partial<ApiResponse> | undefined
    // 未登录/令牌失效统一为 401；服务端仍保留 NEED_LOGIN 供识别
    if (error.response?.status === 401) {
      dispatchNeedLogin()
    }
    if (data && typeof data === 'object') {
      if (data.errorMsg === 'NEED_LOGIN') {
        dispatchNeedLogin()
      }
      if (typeof data.errorMsg === 'string' && data.errorMsg.trim()) {
        return Promise.reject(new Error(data.errorMsg))
      }
    }
    return Promise.reject(new Error(error.message || '请求失败'))
  }
)

export default http
