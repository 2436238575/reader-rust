// Access Token 的自动续期编排。
//
// 服务端把登录拆成两枚令牌：6 小时的 Access Token（JWT）与 7 天的
// Refresh Token（一次性随机串）。这里负责：
// - 401 后单飞刷新并让调用方重放请求；
// - 跨标签页互斥（Web Locks 优先，缺失时退化为 localStorage 软锁），
//   避免两个页面同时消费同一枚 Refresh Token；
// - 到期前 5 分钟主动续期，给已渲染的图片 URL 留出旧令牌的有效余量。
import axios, { AxiosError } from 'axios'
import { API_BASE } from './appBase'
import { notifyNeedLogin } from './authEvents'
import {
  ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY,
  ACCESS_TOKEN_STORAGE_KEY,
  REFRESH_TOKEN_STORAGE_KEY,
  clearAuthTokens,
  readAccessToken,
  readAccessTokenExpiresAt,
  readRefreshToken,
  storeAuthTokens,
} from './secureAccess'

/** 到期前多久主动续期。 */
const REFRESH_AHEAD_MS = 5 * 60 * 1000
/** 主动续期失败后的重试间隔（网络/服务端故障，令牌不丢）。 */
const RETRY_DELAY_MS = 60 * 1000
/** 刷新请求单独设定超时，避免拖住所有等待中的业务请求。 */
const REFRESH_TIMEOUT_MS = 15_000
/** 跨标签页互斥锁的名字。 */
const LOCK_NAME = 'reader-rust-auth-refresh'
/** 无 Web Locks 时的 localStorage 软锁键名。 */
const FALLBACK_LOCK_KEY = 'reader.auth.refresh.lock'
/**
 * 软锁租约时长：持锁标签页崩溃也不会永久卡死其他页面。
 * 租约必须明显大于任务超时（REFRESH_TIMEOUT_MS）：两者相等时，慢刷新占满
 * 整个超时窗口的瞬间租约恰好到期，另一标签页会抢锁并发刷新，把一次性
 * Refresh Token 的轮换变成竞态。
 */
const FALLBACK_LOCK_LEASE_MS = 2 * REFRESH_TIMEOUT_MS
/** 等锁上限：超时退化为无锁直接执行，宁可竞态也不永久卡住用户。 */
const FALLBACK_LOCK_WAIT_MS = 5_000
/** 等锁轮询间隔。 */
const FALLBACK_LOCK_POLL_MS = 300

/** 服务端明确判定会话失效：本地令牌已清空，需要重新登录。 */
export class SessionExpiredError extends Error {
  constructor() {
    super('登录状态已失效')
    this.name = 'SessionExpiredError'
  }
}

/** 刷新请求没能完成（断网 / 5xx）：令牌保留，可稍后重试。 */
export class RefreshNetworkError extends Error {
  constructor(message = '网络异常，请稍后重试') {
    super(message)
    this.name = 'RefreshNetworkError'
  }
}

interface RefreshPayload {
  accessToken: string
  refreshToken: string
  accessTokenExpiresIn: number
}

let inflight: Promise<string> | null = null
let refreshTimer: ReturnType<typeof setTimeout> | null = null
let lifecycleStarted = false

/**
 * 用 Refresh Token 换一对新令牌。
 *
 * `staleAccessToken` 是刚刚收到 401 的请求所用令牌：进锁后如果发现本地
 * 令牌已被其他标签页换掉，直接复用新令牌，不再消费 Refresh Token。
 */
export function refreshAccessToken(staleAccessToken?: string): Promise<string> {
  if (inflight) return inflight
  const task = withCrossTabLock(() => doRefresh(staleAccessToken))
    .catch((error: unknown) => {
      if (error instanceof SessionExpiredError) {
        notifyNeedLogin()
      }
      throw error
    })
    .finally(() => {
      if (inflight === task) inflight = null
    })
  inflight = task
  return task
}

async function doRefresh(staleAccessToken?: string): Promise<string> {
  const current = readAccessToken()
  if (staleAccessToken && current && current !== staleAccessToken) {
    return current
  }
  const refreshToken = readRefreshToken()
  if (!refreshToken) {
    clearAuthTokens()
    throw new SessionExpiredError()
  }

  let response
  try {
    response = await axios.post(
      `${API_BASE}/refreshToken`,
      { refreshToken },
      {
        timeout: REFRESH_TIMEOUT_MS,
        headers: { 'Content-Type': 'application/json' },
      }
    )
  } catch (error) {
    const status = (error as AxiosError)?.response?.status
    // 400/401 是服务端对令牌的明确否定（过期/重放/版本失效），必须重新登录
    if (status === 400 || status === 401) {
      clearAuthTokens()
      throw new SessionExpiredError()
    }
    throw new RefreshNetworkError()
  }

  const payload = response?.data
  const data = payload?.data as Partial<RefreshPayload> | undefined
  if (
    !payload?.isSuccess ||
    typeof data?.accessToken !== 'string' ||
    typeof data.refreshToken !== 'string' ||
    typeof data.accessTokenExpiresIn !== 'number'
  ) {
    clearAuthTokens()
    throw new SessionExpiredError()
  }

  const tokens: RefreshPayload = {
    accessToken: data.accessToken,
    refreshToken: data.refreshToken,
    accessTokenExpiresIn: data.accessTokenExpiresIn,
  }
  storeAuthTokens(tokens)
  scheduleAutoRefresh()
  dispatchTokenChange()
  return tokens.accessToken
}

type LockManagerLike = {
  request<T>(name: string, callback: () => Promise<T>): Promise<T>
}

interface FallbackLockRecord {
  owner: string
  expiresAt: number
}

function withCrossTabLock<T>(task: () => Promise<T>): Promise<T> {
  if (typeof navigator === 'undefined') return task()
  // 用 unknown 中转，避免与 lib.dom 里真正的 LockManager 重载叠加后推断错位
  const locks = (navigator as unknown as { locks?: LockManagerLike }).locks
  if (locks) return locks.request(LOCK_NAME, () => task())
  // 旧 Firefox ESR / 部分 WebView 没有 Web Locks：退化为 localStorage 租约软锁。
  // 刷新令牌是一次性轮换，两个标签页同时 401 时若不加互斥，输家会被误判成
  // 会话失效而清空令牌弹登录；软锁把这种情况尽量串行化。
  return withFallbackSoftLock(task)
}

/**
 * localStorage 租约软锁：写入 {owner, expiresAt}，写后再读确认 owner 是自己
 * 才算拿到锁；拿不到（被他人持有且未超时 / 存储不可用）时退化为无锁直接执行。
 */
async function withFallbackSoftLock<T>(task: () => Promise<T>): Promise<T> {
  if (typeof localStorage === 'undefined') return task()
  const owner = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
  if (!(await acquireFallbackLock(owner))) return task()
  try {
    return await task()
  } finally {
    releaseFallbackLock(owner)
  }
}

function readFallbackLock(): FallbackLockRecord | null {
  try {
    const raw = localStorage.getItem(FALLBACK_LOCK_KEY)
    if (!raw) return null
    const parsed: unknown = JSON.parse(raw)
    if (typeof parsed !== 'object' || parsed === null) return null
    const record = parsed as Partial<FallbackLockRecord>
    if (typeof record.owner !== 'string' || typeof record.expiresAt !== 'number') {
      return null
    }
    return { owner: record.owner, expiresAt: record.expiresAt }
  } catch {
    // 隐私模式读异常 / 内容损坏：按无锁处理，绝不能因此卡死
    return null
  }
}

async function acquireFallbackLock(owner: string): Promise<boolean> {
  const deadline = Date.now() + FALLBACK_LOCK_WAIT_MS
  for (;;) {
    const existing = readFallbackLock()
    const now = Date.now()
    if (!existing || existing.expiresAt <= now || existing.owner === owner) {
      // 无锁 / 锁已过期（持锁方崩溃）/ 锁是自己的：写入自己的租约尝试抢占
      const record: FallbackLockRecord = { owner, expiresAt: now + FALLBACK_LOCK_LEASE_MS }
      try {
        localStorage.setItem(FALLBACK_LOCK_KEY, JSON.stringify(record))
      } catch {
        // 隐私模式写不进去：拿不到锁，但也不要因此卡住用户
        return false
      }
      // 写后读确认：同一瞬间可能有另一个标签页也写入，以最后读到的 owner 为准
      const confirmed = readFallbackLock()
      if (confirmed && confirmed.owner === owner) return true
    }
    if (Date.now() >= deadline) return false
    await new Promise<void>((resolve) => setTimeout(resolve, FALLBACK_LOCK_POLL_MS))
  }
}

function releaseFallbackLock(owner: string) {
  try {
    // 只有 owner 仍是自己时才删除：租约过期后锁可能已被别的标签页抢占
    const current = readFallbackLock()
    if (current && current.owner === owner) {
      localStorage.removeItem(FALLBACK_LOCK_KEY)
    }
  } catch {
    // 删除失败无妨：租约到期后自然失效
  }
}

/**
 * 按本地记录的过期时间安排下一次主动续期。
 *
 * 没有过期时间戳的老会话（升级前登录）会立即换一次，顺带拿到新时间戳。
 */
export function scheduleAutoRefresh() {
  if (typeof window === 'undefined') return
  clearRefreshTimer()
  if (!readRefreshToken()) return

  const expiresAt = readAccessTokenExpiresAt()
  if (!expiresAt) {
    refreshTimer = setTimeout(() => {
      refreshTimer = null
      void runScheduledRefresh()
    }, 0)
    return
  }
  const delay = Math.max(expiresAt - Date.now() - REFRESH_AHEAD_MS, 1_000)
  refreshTimer = setTimeout(() => {
    refreshTimer = null
    void runScheduledRefresh()
  }, delay)
}

async function runScheduledRefresh() {
  if (!readRefreshToken()) return
  const current = readAccessToken()
  try {
    await refreshAccessToken(current)
  } catch (error) {
    if (error instanceof SessionExpiredError) return
    // 网络/服务端故障：令牌还在，稍后重试；离线期间不要把用户登出
    if (readRefreshToken()) {
      clearRefreshTimer()
      refreshTimer = setTimeout(() => {
        refreshTimer = null
        void runScheduledRefresh()
      }, RETRY_DELAY_MS)
    }
  }
}

/** 启动生命周期监听（幂等）。 */
export function startAuthLifecycle() {
  if (typeof window === 'undefined') return
  if (lifecycleStarted) {
    scheduleAutoRefresh()
    return
  }
  lifecycleStarted = true
  window.addEventListener('visibilitychange', handleVisibilityChange)
  window.addEventListener('storage', handleStorageChange)
  scheduleAutoRefresh()
}

/** 登出时停止全部定时器与监听。 */
export function stopAuthLifecycle() {
  lifecycleStarted = false
  clearRefreshTimer()
  if (typeof window !== 'undefined') {
    window.removeEventListener('visibilitychange', handleVisibilityChange)
    window.removeEventListener('storage', handleStorageChange)
  }
}

function handleVisibilityChange() {
  if (document.visibilityState !== 'visible' || !readRefreshToken()) return
  const expiresAt = readAccessTokenExpiresAt()
  if (!expiresAt || expiresAt - Date.now() <= REFRESH_AHEAD_MS) {
    void runScheduledRefresh()
  } else {
    scheduleAutoRefresh()
  }
}

function handleStorageChange(event: StorageEvent) {
  if (
    event.key === ACCESS_TOKEN_STORAGE_KEY ||
    event.key === ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY ||
    event.key === REFRESH_TOKEN_STORAGE_KEY
  ) {
    scheduleAutoRefresh()
  }
}

function clearRefreshTimer() {
  if (refreshTimer) {
    clearTimeout(refreshTimer)
    refreshTimer = null
  }
}

function dispatchTokenChange() {
  if (typeof window !== 'undefined') {
    window.dispatchEvent(new CustomEvent('auth-token-changed'))
  }
}
