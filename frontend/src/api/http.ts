import axios from 'axios'
import type { ApiResponse } from '../types'
import { readAccessToken } from '../utils/secureAccess'
import { API_BASE } from '../utils/appBase'

let lastNeedLoginDispatchAt = 0

function dispatchNeedLogin() {
  const now = Date.now()
  if (now - lastNeedLoginDispatchAt < 1500) return
  lastNeedLoginDispatchAt = now
  window.dispatchEvent(new CustomEvent('need-login'))
}

const http = axios.create({
  baseURL: API_BASE,
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
      return Promise.reject(new Error(data.errorMsg || '请求失败'))
    }
    // Return unwrapped data
    response.data = data.data
    return response
  },
  (error) => {
    const data = error.response?.data as Partial<ApiResponse> | undefined
    // 未登录/令牌失效统一为 HTTP 401，据此拉起登录框；
    // errorMsg 是用户可读的文案，直接作为错误信息抛出
    if (error.response?.status === 401) {
      dispatchNeedLogin()
    }
    if (data && typeof data === 'object') {
      if (typeof data.errorMsg === 'string' && data.errorMsg.trim()) {
        return Promise.reject(new Error(data.errorMsg))
      }
    }
    return Promise.reject(new Error(error.message || '请求失败'))
  }
)

export default http
