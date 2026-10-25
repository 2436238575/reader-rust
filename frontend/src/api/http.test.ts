// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { AxiosResponse, InternalAxiosRequestConfig } from 'axios'
import http from './http'
import {
  RefreshNetworkError,
  SessionExpiredError,
  refreshAccessToken,
} from '../utils/authSession'
import { storeAuthHint } from '../utils/secureAccess'

vi.mock('../utils/authSession', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../utils/authSession')>()
  return { ...actual, refreshAccessToken: vi.fn() }
})

const refreshMock = vi.mocked(refreshAccessToken)
const originalAdapter = http.defaults.adapter

function unauthorized(config: InternalAxiosRequestConfig): Promise<never> {
  return Promise.reject({
    message: 'Unauthorized',
    isAxiosError: true,
    config,
    response: {
      status: 401,
      statusText: 'Unauthorized',
      headers: {},
      config,
      data: { isSuccess: false, errorMsg: '需要登录', data: null },
    },
  })
}

function ok(config: InternalAxiosRequestConfig): AxiosResponse {
  return {
    data: { isSuccess: true, errorMsg: '', data: { ok: true } },
    status: 200,
    statusText: 'OK',
    headers: {},
    config,
  }
}

function seedHint() {
  storeAuthHint({ username: 'admin', accessTokenExpiresAt: Date.now() + 3600 * 1000 })
}

beforeEach(() => {
  localStorage.clear()
  refreshMock.mockReset()
})

afterEach(() => {
  http.defaults.adapter = originalAdapter
  localStorage.clear()
  vi.restoreAllMocks()
})

describe('http auth interceptor', () => {
  it('401 后刷新会话并重放一次；请求全程不带 Authorization，Cookie 由浏览器携带', async () => {
    seedHint()
    refreshMock.mockResolvedValue(undefined)
    const seen: Array<string | undefined> = []
    const withCredentials: boolean[] = []
    let calls = 0
    http.defaults.adapter = async (config) => {
      calls += 1
      seen.push(String(config.headers?.Authorization ?? ''))
      withCredentials.push(config.withCredentials === true)
      return calls === 1 ? unauthorized(config) : ok(config)
    }

    const response = await http.get('/getBookshelf')

    expect(response.data).toEqual({ ok: true })
    expect(refreshMock).toHaveBeenCalledTimes(1)
    expect(calls).toBe(2)
    // 请求拦截器不再注入 Bearer：两次请求都没有 Authorization 头
    expect(seen).toEqual(['', ''])
    // 会话凭据走 Cookie：withCredentials 必须打开
    expect(withCredentials).toEqual([true, true])
  })

  it('retries a 401 only once even if the replay also fails', async () => {
    seedHint()
    refreshMock.mockResolvedValue(undefined)
    let calls = 0
    http.defaults.adapter = async (config) => {
      calls += 1
      return unauthorized(config)
    }

    await expect(http.get('/getBookshelf')).rejects.toThrow('需要登录')
    expect(calls).toBe(2)
    expect(refreshMock).toHaveBeenCalledTimes(1)
  })

  it('reports expiry when the refresh is rejected', async () => {
    seedHint()
    refreshMock.mockRejectedValue(new SessionExpiredError())
    let calls = 0
    http.defaults.adapter = async (config) => {
      calls += 1
      return unauthorized(config)
    }

    await expect(http.get('/getBookshelf')).rejects.toThrow('需要登录')
    expect(calls).toBe(1)
  })

  it('keeps the network error message when the refresh cannot reach the server', async () => {
    seedHint()
    refreshMock.mockRejectedValue(new RefreshNetworkError())
    http.defaults.adapter = async (config) => unauthorized(config)

    await expect(http.get('/getBookshelf')).rejects.toThrow('网络异常，请稍后重试')
  })

  it('does not attempt a refresh without an auth hint', async () => {
    http.defaults.adapter = async (config) => unauthorized(config)

    await expect(http.get('/getBookshelf')).rejects.toThrow('需要登录')
    expect(refreshMock).not.toHaveBeenCalled()
  })

  it('never refreshes the login endpoint itself', async () => {
    seedHint()
    http.defaults.adapter = async (config) => unauthorized(config)

    await expect(http.post('/login', { username: 'a', password: 'b' })).rejects.toThrow(
      '需要登录'
    )
    expect(refreshMock).not.toHaveBeenCalled()
  })
})
