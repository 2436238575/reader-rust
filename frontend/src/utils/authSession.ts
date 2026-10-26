// Cookie 会话的自动续期编排。
//
// Access/Refresh Token 都在 HttpOnly Cookie 里，由浏览器自动携带，前端
// 不再读写任何令牌。这里负责：
// - 401 后单飞刷新（POST /refreshToken，凭 Cookie）并让调用方重放请求；
// - 跨标签页互斥（Web Locks 优先，缺失时退化为 localStorage 软锁），
//   避免两个页面同时刷新造成 Cookie 轮换竞态；
// - 到期前 5 分钟主动续期（依据非敏感 hint 里的过期时间）；
// - 页面重新可见/聚焦时核对过期时间，必要时提前续期。
import axios, { AxiosError } from 'axios'
import { API_BASE } from './appBase'
import { notifyNeedLogin } from './authEvents'
import {
  AUTH_HINT_STORAGE_KEY,
  clearAuthHint,
  hasAuthHint,
  readAccessTokenExpiresAt,
  readAuthHint,
  storeAuthHint,
} from './secureAccess'

/** 到期前多久主动续期。 */
const REFRESH_AHEAD_MS = 5 * 60 * 1000
/** 主动续期失败后的重试间隔（网络/服务端故障，会话不丢）。 */
const RETRY_DELAY_MS = 60 * 1000
/** 刷新请求单独设定超时，避免拖住所有等待中的业务请求。 */
const REFRESH_TIMEOUT_MS = 15_000
/**
 * 刚刷新成功后的短路窗口。Cookie 会话拿不到「新令牌值」可以做比对，
 * 用时间戳代替：短窗口内的再次 401 多为并发时序抖动，直接复用刚换的
 * Cookie，不再打刷新接口。
 */
const REFRESH_SUCCESS_DEDUPE_MS = 30_000
/** 跨标签页互斥锁的名字。 */
const LOCK_NAME = 'reader-rust-auth-refresh'
/** 无 Web Locks 时的 localStorage 软锁键名。 */
const FALLBACK_LOCK_KEY = 'reader.auth.refresh.lock'
/**
 * 软锁租约时长：持锁标签页崩溃也不会永久卡死其他页面。
 * 租约必须明显大于任务超时（REFRESH_TIMEOUT_MS）：两者相等时，慢刷新占满
 * 整个超时窗口的瞬间租约恰好到期，另一标签页会抢锁并发刷新，把一次性
 * 轮换的刷新变成竞态。
 */
const FALLBACK_LOCK_LEASE_MS = 2 * REFRESH_TIMEOUT_MS
/** 等锁上限：超时退化为无锁直接执行，宁可竞态也不永久卡住用户。 */
const FALLBACK_LOCK_WAIT_MS = 5_000
/** 等锁轮询间隔。 */
const FALLBACK_LOCK_POLL_MS = 300

/** 服务端明确判定会话失效：本地登录提示已清空，需要重新登录。 */
export class SessionExpiredError extends Error {
  constructor() {
    super('登录状态已失效')
    this.name = 'SessionExpiredError'
  }
}

/** 刷新请求没能完成（断网 / 5xx）：会话保留，可稍后重试。 */
export class RefreshNetworkError extends Error {
  constructor(message = '网络异常，请稍后重试') {
    super(message)
    this.name = 'RefreshNetworkError'
  }
}

interface RefreshPayload {
  accessTokenExpiresIn: number
}

let inflight: Promise<void> | null = null
let refreshTimer: ReturnType<typeof setTimeout> | null = null
let lifecycleStarted = false
/** 上次刷新成功的时间戳（Date.now()）；用于短路短窗口内的重复刷新。 */
let lastRefreshAt = 0

/**
 * 凭 Cookie 刷新会话：成功后服务端换发两枚 Cookie，这里只更新 hint 里的
 * 过期时间（用户名保留，首刷时可能还没有用户名）。
 */
export function refreshAccessToken(): Promise<void> {
  if (inflight) return inflight
  const task = withCrossTabLock(() => doRefresh())
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

async function doRefresh(): Promise<void> {
  // 刚刷新成功过：新 Cookie 已生效，短窗口内的再次 401 多为并发时序，
  // 直接短路返回，避免把续期接口打成风暴
  if (Date.now() - lastRefreshAt < REFRESH_SUCCESS_DEDUPE_MS) return

  let response
  try {
    // 服务端只认 reader_refresh Cookie，body 恒为空对象
    response = await axios.post(`${API_BASE}/refreshToken`, {}, {
      timeout: REFRESH_TIMEOUT_MS,
      // 刷新必须带上 reader_refresh Cookie；跨域 CORS 部署同样要发凭据
      withCredentials: true,
      headers: { 'Content-Type': 'application/json' },
    })
  } catch (error) {
    const status = (error as AxiosError)?.response?.status
    // 400/401 是服务端对会话的明确否定（Cookie 缺失/过期/重放），必须重新登录
    if (status === 400 || status === 401) {
      clearAuthHint()
      throw new SessionExpiredError()
    }
    throw new RefreshNetworkError()
  }

  const payload = response?.data
  const data = payload?.data as Partial<RefreshPayload> | undefined
  if (!payload?.isSuccess || typeof data?.accessTokenExpiresIn !== 'number') {
    clearAuthHint()
    throw new SessionExpiredError()
  }

  lastRefreshAt = Date.now()
  const hint = readAuthHint()
  storeAuthHint({
    username: hint?.username ?? '',
    accessTokenExpiresAt: Date.now() + data.accessTokenExpiresIn * 1000,
  })
  scheduleAutoRefresh()
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
  // 刷新是一次性轮换，两个标签页同时 401 时若不加互斥，输家会被误判成
  // 会话失效而清提示弹登录；软锁把这种情况尽量串行化。
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
 * hint 里没有过期时间戳（异常数据）会立即换一次，顺带拿回新时间戳。
 */
export function scheduleAutoRefresh() {
  if (typeof window === 'undefined') return
  clearRefreshTimer()
  if (!hasAuthHint()) return

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
  if (!hasAuthHint()) return
  try {
    await refreshAccessToken()
  } catch (error) {
    if (error instanceof SessionExpiredError) return
    // 网络/服务端故障：会话还在，稍后重试；离线期间不要把用户登出
    if (hasAuthHint()) {
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
  // 桌面端切回标签页不一定触发 visibilitychange（如同一窗口内点击），
  // focus 兜底一次到期核对
  window.addEventListener('focus', handleResume)
  window.addEventListener('storage', handleStorageChange)
  scheduleAutoRefresh()
}

/** 登出时停止全部定时器与监听。 */
export function stopAuthLifecycle() {
  lifecycleStarted = false
  clearRefreshTimer()
  // 短路时间戳随会话一起作废：新会话不该被旧会话的刷新记录挡住
  lastRefreshAt = 0
  if (typeof window !== 'undefined') {
    window.removeEventListener('visibilitychange', handleVisibilityChange)
    window.removeEventListener('focus', handleResume)
    window.removeEventListener('storage', handleStorageChange)
  }
}

function handleVisibilityChange() {
  if (document.visibilityState !== 'visible') return
  handleResume()
}

/** 页面恢复可见/聚焦：隔了很久时先核对到期时间，临近到期就先续上。 */
function handleResume() {
  if (!hasAuthHint()) return
  const expiresAt = readAccessTokenExpiresAt()
  if (!expiresAt || expiresAt - Date.now() <= REFRESH_AHEAD_MS) {
    void runScheduledRefresh()
  } else {
    scheduleAutoRefresh()
  }
}

function handleStorageChange(event: StorageEvent) {
  // 其他标签页刷新成功会更新 hint：按新过期时间重排本页的下一次续期
  if (event.key === AUTH_HINT_STORAGE_KEY) {
    scheduleAutoRefresh()
  }
}

function clearRefreshTimer() {
  if (refreshTimer) {
    clearTimeout(refreshTimer)
    refreshTimer = null
  }
}
