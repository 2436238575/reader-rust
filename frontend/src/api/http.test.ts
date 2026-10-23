// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { AxiosResponse, InternalAxiosRequestConfig } from 'axios'
import http from './http'
import {
  RefreshNetworkError,
  SessionExpiredError,
  refreshAccessToken,
} from '../utils/authSession'

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

function seedTokens() {
  localStorage.setItem('accessToken', 'old-access')
  localStorage.setItem('refreshToken', 'rt_old')
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
  it('refreshes on 401 and replays the original request with the new token', async () => {
    seedTokens()
    refreshMock.mockImplementation(async () => {
      localStorage.setItem('accessToken', 'fresh-access')
      localStorage.setItem('refreshToken', 'rt_fresh')
      return 'fresh-access'
    })
    const seen: string[] = []
    http.defaults.adapter = async (config) => {
      seen.push(String(config.headers.Authorization ?? ''))
      return seen.length === 1 ? unauthorized(config) : ok(config)
    }

    const response = await http.get('/getBookshelf')

    expect(response.data).toEqual({ ok: true })
    expect(refreshMock).toHaveBeenCalledTimes(1)
    expect(seen).toEqual(['Bearer old-access', 'Bearer fresh-access'])
  })

  it('retries a 401 only once even if the replay also fails', async () => {
    seedTokens()
    refreshMock.mockResolvedValue('fresh-access')
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
    seedTokens()
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
    seedTokens()
    refreshMock.mockRejectedValue(new RefreshNetworkError())
    http.defaults.adapter = async (config) => unauthorized(config)

    await expect(http.get('/getBookshelf')).rejects.toThrow('网络异常，请稍后重试')
  })

  it('does not attempt a refresh without a refresh token', async () => {
    localStorage.setItem('accessToken', 'old-access')
    http.defaults.adapter = async (config) => unauthorized(config)

    await expect(http.get('/getBookshelf')).rejects.toThrow('需要登录')
    expect(refreshMock).not.toHaveBeenCalled()
  })

  it('never refreshes the login endpoint itself', async () => {
    localStorage.setItem('refreshToken', 'rt_old')
    http.defaults.adapter = async (config) => unauthorized(config)

    await expect(http.post('/login', { username: 'a', password: 'b' })).rejects.toThrow(
      '需要登录'
    )
    expect(refreshMock).not.toHaveBeenCalled()
  })
})
