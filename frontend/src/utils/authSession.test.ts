// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import axios from 'axios'
import { API_BASE } from './appBase'
import {
  RefreshNetworkError,
  SessionExpiredError,
  refreshAccessToken,
  scheduleAutoRefresh,
  startAuthLifecycle,
  stopAuthLifecycle,
} from './authSession'
import {
  AUTH_HINT_STORAGE_KEY,
  LEGACY_REFRESH_TOKEN_STORAGE_KEY,
  hasAuthHint,
  readAuthHint,
  readLegacyRefreshToken,
  storeAuthHint,
} from './secureAccess'

function refreshResponse(accessTokenExpiresIn = 21600) {
  return {
    data: {
      isSuccess: true,
      errorMsg: '',
      // 新契约：刷新响应只有 accessToken 与 accessTokenExpiresIn，不再回 refreshToken
      data: { accessToken: 'new-access', accessTokenExpiresIn },
    },
  }
}

function seedHint(username = 'alice', expiresInSecs = 6 * 3600) {
  storeAuthHint({ username, accessTokenExpiresAt: Date.now() + expiresInSecs * 1000 })
}

beforeEach(() => {
  localStorage.clear()
  vi.restoreAllMocks()
})

afterEach(() => {
  // stopAuthLifecycle 同时复位刷新短路的 lastRefreshAt，避免用例间串扰
  stopAuthLifecycle()
  vi.useRealTimers()
  vi.restoreAllMocks()
  localStorage.clear()
})

describe('authSession Cookie 刷新', () => {
  it('成功：body 为空对象、withCredentials 打开，更新 hint 过期时间并保留用户名', async () => {
    seedHint('alice', 1)
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())

    await expect(refreshAccessToken()).resolves.toBeUndefined()

    expect(post).toHaveBeenCalledTimes(1)
    const [url, body, config] = post.mock.calls[0]
    expect(url).toBe(`${API_BASE}/refreshToken`)
    expect(body).toEqual({})
    expect(config?.withCredentials).toBe(true)

    const hint = readAuthHint()
    expect(hint?.username).toBe('alice')
    expect(hint?.accessTokenExpiresAt).toBeGreaterThan(Date.now() + 5 * 3600 * 1000)
  })

  it('旧版 localStorage Refresh Token 会放进 body 一次（迁移）', async () => {
    seedHint()
    localStorage.setItem(LEGACY_REFRESH_TOKEN_STORAGE_KEY, 'rt_old')
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())

    await refreshAccessToken()

    expect(post.mock.calls[0][1]).toEqual({ refreshToken: 'rt_old' })
    // 旧键的删除由 bootstrapAuth 负责，这里只负责带上
    expect(readLegacyRefreshToken()).toBe('rt_old')
  })

  it('并发刷新合并为一次请求', async () => {
    seedHint()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())

    await Promise.all([refreshAccessToken(), refreshAccessToken()])

    expect(post).toHaveBeenCalledTimes(1)
  })

  it('刚刷新成功后短窗口内短路，不再打刷新接口', async () => {
    seedHint()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())

    await refreshAccessToken()
    await refreshAccessToken()

    expect(post).toHaveBeenCalledTimes(1)
  })

  it('400/401 是对会话的明确否定：清 hint 并抛 SessionExpiredError', async () => {
    seedHint()
    vi.spyOn(axios, 'post').mockRejectedValue({ response: { status: 401 } })

    await expect(refreshAccessToken()).rejects.toBeInstanceOf(SessionExpiredError)
    expect(hasAuthHint()).toBe(false)
  })

  it('响应 isSuccess=false 同样视为会话失效', async () => {
    seedHint()
    vi.spyOn(axios, 'post').mockResolvedValue({
      data: { isSuccess: false, errorMsg: '令牌无效', data: null },
    })

    await expect(refreshAccessToken()).rejects.toBeInstanceOf(SessionExpiredError)
    expect(hasAuthHint()).toBe(false)
  })

  it('网络失败抛 RefreshNetworkError 并保留 hint', async () => {
    seedHint()
    vi.spyOn(axios, 'post').mockRejectedValue(new Error('offline'))

    await expect(refreshAccessToken()).rejects.toBeInstanceOf(RefreshNetworkError)
    expect(hasAuthHint()).toBe(true)
    expect(readAuthHint()?.username).toBe('alice')
  })

  it('到期前 5 分钟主动续期', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    seedHint('alice', 10 * 60)
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())

    scheduleAutoRefresh()
    await vi.advanceTimersByTimeAsync(5 * 60 * 1000)

    expect(post).toHaveBeenCalledTimes(1)
    expect(readAuthHint()?.accessTokenExpiresAt).toBe(Date.now() + 21600 * 1000)
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
    seedHint()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())
    localStorage.setItem(
      LOCK_KEY,
      JSON.stringify({ owner: 'other-tab', expiresAt: Date.now() + 10_000 })
    )

    const pending = refreshAccessToken()

    // 他人持锁期间：轮询几个周期也不会发起刷新请求
    await vi.advanceTimersByTimeAsync(900)
    expect(post).not.toHaveBeenCalled()

    // 另一个标签页释放锁后，本轮刷新拿到锁并执行
    localStorage.removeItem(LOCK_KEY)
    await vi.advanceTimersByTimeAsync(300)

    await expect(pending).resolves.toBeUndefined()
    expect(post).toHaveBeenCalledTimes(1)
    // 刷新完成后软锁已主动释放
    expect(localStorage.getItem(LOCK_KEY)).toBeNull()
  })

  it('过期的软锁会被直接抢占，不等待', async () => {
    vi.useFakeTimers()
    seedHint()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())
    localStorage.setItem(
      LOCK_KEY,
      JSON.stringify({ owner: 'crashed-tab', expiresAt: Date.now() - 1 })
    )

    await expect(refreshAccessToken()).resolves.toBeUndefined()
    expect(post).toHaveBeenCalledTimes(1)
    expect(localStorage.getItem(LOCK_KEY)).toBeNull()
  })

  it('软锁内容损坏（非法 JSON）时按无锁处理，不会卡死', async () => {
    vi.useFakeTimers()
    seedHint()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())
    localStorage.setItem(LOCK_KEY, '{not-json')

    await expect(refreshAccessToken()).resolves.toBeUndefined()
    expect(post).toHaveBeenCalledTimes(1)
    expect(localStorage.getItem(LOCK_KEY)).toBeNull()
  })

  it('localStorage 写失败（隐私模式）时退化为无锁直接刷新', async () => {
    seedHint()
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())
    const original = Storage.prototype.setItem
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(function (
      key: string,
      value: string
    ) {
      if (key === LOCK_KEY) throw new Error('QuotaExceededError')
      original.call(window.localStorage, key, value)
    })

    await expect(refreshAccessToken()).resolves.toBeUndefined()
    expect(post).toHaveBeenCalledTimes(1)
    expect(hasAuthHint()).toBe(true)
  })

  it('软锁租约大于刷新超时：15s 时锁仍属于原 owner，30s 后才可被抢占', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    seedHint()
    // 慢刷新：请求挂起，持锁标签页在整个超时窗口内一直占着锁
    let rejectPost: (error: Error) => void = () => {}
    const post = vi.spyOn(axios, 'post').mockImplementation(
      () =>
        new Promise<never>((_, reject) => {
          rejectPost = reject
        })
    )

    const pending = refreshAccessToken()
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

  it('其他标签页刷新成功（storage 事件更新 hint）会重排本页续期计划', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    seedHint('alice', 30 * 60)
    const post = vi.spyOn(axios, 'post').mockResolvedValue(refreshResponse())

    // 启动生命周期：注册 storage 监听并按 30 分钟过期排出 25 分钟后的续期
    startAuthLifecycle()
    // 其他标签页刚刷新把过期推到 6 小时后（真实场景里 storage 先被写入、
    // 随后本页收到 storage 事件）
    storeAuthHint({ username: 'alice', accessTokenExpiresAt: Date.now() + 6 * 3600 * 1000 })
    window.dispatchEvent(
      new StorageEvent('storage', {
        key: AUTH_HINT_STORAGE_KEY,
        newValue: localStorage.getItem(AUTH_HINT_STORAGE_KEY),
      })
    )
    // 旧计划（25 分钟后）不再触发
    await vi.advanceTimersByTimeAsync(25 * 60 * 1000)
    expect(post).not.toHaveBeenCalled()

    // 新计划（6h - 5min 后）到点触发
    await vi.advanceTimersByTimeAsync((6 * 3600 - 5 * 60) * 1000)
    expect(post).toHaveBeenCalledTimes(1)
  })
})
