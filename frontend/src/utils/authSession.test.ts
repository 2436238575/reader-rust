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

describe('authSession 跨标签页软锁回退（无 navigator.locks）', () => {
  const LOCK_KEY = 'reader.auth.refresh.lock'
  // 与 authSession.ts 内部的锁记录结构一致；类型不导出，这里本地声明
  interface LockRecord {
    owner: string
    expiresAt: number
  }

  // jsdom 没有 Web Locks API，回退路径本就会被走到；这里再显式把
  // navigator.locks 置为 undefined，保证未来测试环境实现了 locks 时，
  // 这组用例仍然针对软锁回退。
  beforeEach(() => {
    Object.defineProperty(window.navigator, 'locks', {
      value: undefined,
      configurable: true,
      writable: true,
    })
  })

  afterEach(() => {
    // 删掉自有属性，恢复原型链上可能存在的原生定义
    delete (window.navigator as { locks?: unknown }).locks
  })

  it('其他标签页持有的有效软锁会让刷新等待，锁释放后才执行', async () => {
    vi.useFakeTimers()
    seedTokens()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())
    localStorage.setItem(
      LOCK_KEY,
      JSON.stringify({ owner: 'other-tab', expiresAt: Date.now() + 10_000 })
    )

    const pending = refreshAccessToken('old-access')

    // 他人持锁期间：轮询几个周期也不会发起刷新请求
    await vi.advanceTimersByTimeAsync(900)
    expect(post).not.toHaveBeenCalled()

    // 另一个标签页释放锁后，本轮刷新拿到锁并执行
    localStorage.removeItem(LOCK_KEY)
    await vi.advanceTimersByTimeAsync(300)

    await expect(pending).resolves.toBe('new-access')
    expect(post).toHaveBeenCalledTimes(1)
    // 刷新完成后软锁已主动释放
    expect(localStorage.getItem(LOCK_KEY)).toBeNull()
  })

  it('过期的软锁会被直接抢占，不等待', async () => {
    vi.useFakeTimers()
    seedTokens()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())
    localStorage.setItem(
      LOCK_KEY,
      JSON.stringify({ owner: 'crashed-tab', expiresAt: Date.now() - 1 })
    )

    await expect(refreshAccessToken('old-access')).resolves.toBe('new-access')
    expect(post).toHaveBeenCalledTimes(1)
    expect(localStorage.getItem(LOCK_KEY)).toBeNull()
  })

  it('软锁内容损坏（非法 JSON）时按无锁处理，不会卡死', async () => {
    vi.useFakeTimers()
    seedTokens()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())
    localStorage.setItem(LOCK_KEY, '{not-json')

    await expect(refreshAccessToken('old-access')).resolves.toBe('new-access')
    expect(post).toHaveBeenCalledTimes(1)
    expect(localStorage.getItem(LOCK_KEY)).toBeNull()
  })

  it('localStorage 写失败（隐私模式）时退化为无锁直接刷新', async () => {
    seedTokens()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())
    const original = Storage.prototype.setItem
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(function (
      key: string,
      value: string
    ) {
      if (key === LOCK_KEY) throw new Error('QuotaExceededError')
      original.call(window.localStorage, key, value)
    })

    await expect(refreshAccessToken('old-access')).resolves.toBe('new-access')
    expect(post).toHaveBeenCalledTimes(1)
    expect(readAccessToken()).toBe('new-access')
  })

  it('软锁租约大于刷新超时：15s 时锁仍属于原 owner，30s 后才可被抢占', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    seedTokens()
    // 慢刷新：请求挂起，持锁标签页在整个超时窗口内一直占着锁
    let rejectPost: (error: Error) => void = () => {}
    const post = vi.spyOn(axios, 'post').mockImplementation(
      () =>
        new Promise<never>((_, reject) => {
          rejectPost = reject
        })
    )

    const pending = refreshAccessToken('old-access')
    await vi.advanceTimersByTimeAsync(0)
    expect(post).toHaveBeenCalledTimes(1)

    const held = JSON.parse(localStorage.getItem(LOCK_KEY)!) as LockRecord
    // 租约 30s = 2×刷新超时（15s）
    expect(held.expiresAt - Date.now()).toBe(30_000)

    // 刷新超时点（15s）：租约未到期，锁仍属于原 owner，其他标签页不能抢占
    await vi.advanceTimersByTimeAsync(15_000)
    const atTimeout = JSON.parse(localStorage.getItem(LOCK_KEY)!) as LockRecord
    expect(atTimeout.owner).toBe(held.owner)
    expect(atTimeout.expiresAt).toBeGreaterThan(Date.now())

    // 30s 后租约才到期，此刻起其他标签页允许抢占（acquire 条件 expiresAt <= now）
    await vi.advanceTimersByTimeAsync(15_000)
    const atExpiry = JSON.parse(localStorage.getItem(LOCK_KEY)!) as LockRecord
    expect(atExpiry.owner).toBe(held.owner)
    expect(atExpiry.expiresAt).toBeLessThanOrEqual(Date.now())

    // 收尾：让挂起的刷新失败落地，避免模块级 inflight 泄漏到后续用例
    rejectPost(new Error('offline'))
    await expect(pending).rejects.toBeInstanceOf(RefreshNetworkError)
  })
})
