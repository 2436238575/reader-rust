// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import axios from 'axios'
import {
  RefreshNetworkError,
  SessionExpiredError,
  refreshAccessToken,
  scheduleAutoRefresh,
  stopAuthLifecycle,
} from './authSession'
import {
  ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY,
  readAccessToken,
  readAccessTokenExpiresAt,
  readRefreshToken,
} from './secureAccess'

function refreshResponse(accessToken = 'new-access', refreshToken = 'rt_new') {
  return {
    data: {
      isSuccess: true,
      errorMsg: '',
      data: { accessToken, refreshToken, accessTokenExpiresIn: 21600 },
    },
  }
}

function seedTokens() {
  localStorage.setItem('accessToken', 'old-access')
  localStorage.setItem('refreshToken', 'rt_old')
}

beforeEach(() => {
  localStorage.clear()
  vi.restoreAllMocks()
})

afterEach(() => {
  stopAuthLifecycle()
  vi.useRealTimers()
  vi.restoreAllMocks()
  localStorage.clear()
})

describe('authSession', () => {
  it('coalesces concurrent refreshes into a single request', async () => {
    seedTokens()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())

    const [first, second] = await Promise.all([
      refreshAccessToken('old-access'),
      refreshAccessToken('old-access'),
    ])

    expect(first).toBe('new-access')
    expect(second).toBe('new-access')
    expect(post).toHaveBeenCalledTimes(1)
    expect(readAccessToken()).toBe('new-access')
    expect(readRefreshToken()).toBe('rt_new')
    expect(readAccessTokenExpiresAt()).toBeGreaterThan(Date.now())
  })

  it('reuses a token that another tab already rotated', async () => {
    localStorage.setItem('accessToken', 'already-refreshed')
    localStorage.setItem('refreshToken', 'rt_current')
    const post = vi.spyOn(axios, 'post')

    const token = await refreshAccessToken('stale-access')

    expect(token).toBe('already-refreshed')
    expect(post).not.toHaveBeenCalled()
  })

  it('clears tokens when the server rejects the refresh token', async () => {
    seedTokens()
    vi.spyOn(axios, 'post').mockRejectedValue({ response: { status: 401 } })

    await expect(refreshAccessToken('old-access')).rejects.toBeInstanceOf(SessionExpiredError)
    expect(readAccessToken()).toBeUndefined()
    expect(readRefreshToken()).toBeUndefined()
  })

  it('keeps tokens when the refresh request fails at the network layer', async () => {
    seedTokens()
    vi.spyOn(axios, 'post').mockRejectedValue(new Error('offline'))

    await expect(refreshAccessToken('old-access')).rejects.toBeInstanceOf(RefreshNetworkError)
    expect(readAccessToken()).toBe('old-access')
    expect(readRefreshToken()).toBe('rt_old')
  })

  it('treats a missing refresh token as an expired session', async () => {
    localStorage.setItem('accessToken', 'old-access')

    await expect(refreshAccessToken('old-access')).rejects.toBeInstanceOf(SessionExpiredError)
    expect(readAccessToken()).toBeUndefined()
  })

  it('refreshes proactively five minutes before the stored expiry', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    seedTokens()
    localStorage.setItem(
      ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY,
      String(Date.now() + 10 * 60 * 1000)
    )
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())

    scheduleAutoRefresh()
    await vi.advanceTimersByTimeAsync(5 * 60 * 1000)

    expect(post).toHaveBeenCalledTimes(1)
    expect(readAccessToken()).toBe('new-access')
  })
})
