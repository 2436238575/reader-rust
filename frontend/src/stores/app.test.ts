// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useAppStore } from './app'
import { getUserInfo } from '../api/user'
import { refreshAccessToken } from '../utils/authSession'
import {
  LEGACY_ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY,
  LEGACY_ACCESS_TOKEN_STORAGE_KEY,
  LEGACY_REFRESH_TOKEN_STORAGE_KEY,
  clearAuthHint,
  hasAuthHint,
  readAuthHint,
  storeAuthHint,
} from '../utils/secureAccess'
import type { UserInfo } from '../types'

vi.mock('../api/user', () => ({
  getUserInfo: vi.fn(),
}))

// syncStatsOnce 背后的后端阅读统计同步：单测不真发请求
vi.mock('../utils/readingStats', () => ({
  loadLocalReadingStats: () => ({
    totalSeconds: 0,
    openedBooks: [],
    readChapters: [],
    completedBooks: [],
  }),
  persistLocalReadingStats: vi.fn(),
  pushReadingStatsToBackend: vi.fn(),
  syncReadingStatsFromBackend: vi.fn(async (stats: unknown) => stats),
  MAX_READ_CHAPTER_ENTRIES: 5000,
  STATS_PUSH_DEBOUNCE_MS: 2000,
}))

// startAuthLifecycle 会挂定时器，单测不需要真实的续期编排；
// SessionExpiredError 与真实实现保持同名同形态，供 bootstrap 分支判断
vi.mock('../utils/authSession', () => ({
  SessionExpiredError: class SessionExpiredError extends Error {
    constructor() {
      super('登录状态已失效')
      this.name = 'SessionExpiredError'
    }
  },
  RefreshNetworkError: class RefreshNetworkError extends Error {
    constructor(message = '网络异常，请稍后重试') {
      super(message)
      this.name = 'RefreshNetworkError'
    }
  },
  refreshAccessToken: vi.fn(),
  scheduleAutoRefresh: vi.fn(),
  startAuthLifecycle: vi.fn(),
  stopAuthLifecycle: vi.fn(),
}))

vi.mock('../utils/systemUi', () => ({
  applySystemTheme: vi.fn(),
}))

const getUserInfoMock = vi.mocked(getUserInfo)
const refreshMock = vi.mocked(refreshAccessToken)

function createUserInfo(username = 'admin'): UserInfo {
  return { username }
}

/** 模拟一次已登录的 Cookie 会话：本地有非敏感登录提示。 */
function seedHint(username = 'admin') {
  storeAuthHint({ username, accessTokenExpiresAt: Date.now() + 6 * 3600 * 1000 })
}

/** 模拟旧版（localStorage 令牌时代）的存量会话。 */
function seedLegacyTokens() {
  localStorage.setItem(LEGACY_ACCESS_TOKEN_STORAGE_KEY, 'old-access')
  localStorage.setItem(LEGACY_REFRESH_TOKEN_STORAGE_KEY, 'rt_old')
  localStorage.setItem(LEGACY_ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY, String(Date.now() + 1000))
}

beforeEach(() => {
  localStorage.clear()
  setActivePinia(createPinia())
  getUserInfoMock.mockReset()
  refreshMock.mockReset()
})

describe('app store fetchUserInfo 合并与新鲜期', () => {
  it('并发调用合并为一次请求', async () => {
    let release!: (value: { userInfo: UserInfo | null }) => void
    getUserInfoMock.mockImplementation(
      () =>
        new Promise<{ userInfo: UserInfo | null }>((resolve) => {
          release = resolve
        })
    )
    const store = useAppStore()
    const first = store.fetchUserInfo()
    const second = store.fetchUserInfo()
    release({ userInfo: createUserInfo() })
    await Promise.all([first, second])

    expect(getUserInfoMock).toHaveBeenCalledTimes(1)
    expect(store.isLoggedIn).toBe(true)
  })

  it('新鲜期内重复调用不再请求，force 绕过', async () => {
    getUserInfoMock.mockResolvedValue({ userInfo: createUserInfo() })
    const store = useAppStore()

    await store.fetchUserInfo()
    await store.fetchUserInfo()
    expect(getUserInfoMock).toHaveBeenCalledTimes(1)

    // need-login、登出后核验走 force：必须重新请求
    await store.fetchUserInfo({ force: true })
    expect(getUserInfoMock).toHaveBeenCalledTimes(2)
  })

  it('网络失败不记新鲜期，下次仍会重试', async () => {
    getUserInfoMock.mockRejectedValueOnce(new Error('网络错误'))
    getUserInfoMock.mockResolvedValue({ userInfo: createUserInfo() })
    const store = useAppStore()

    await store.fetchUserInfo()
    expect(store.isLoggedIn).toBe(false)

    await store.fetchUserInfo()
    expect(getUserInfoMock).toHaveBeenCalledTimes(2)
    expect(store.isLoggedIn).toBe(true)
  })

  it('网络失败但存在登录提示时保留离线登录态', async () => {
    const store = useAppStore()
    // 模拟已经离线启动（bootstrap 依据 hint 保持登录态）后，页面又调了一次
    // fetchUserInfo：网络失败不能把登录态翻回未登录
    store.setUser(createUserInfo())
    getUserInfoMock.mockRejectedValue(new Error('网络错误'))

    await store.fetchUserInfo({ force: true })

    expect(store.isLoggedIn).toBe(true)
  })

  it('setUser 写入服务端用户信息后，fetchUserInfo 直接复用不再请求', async () => {
    const store = useAppStore()
    store.setUser(createUserInfo())

    await store.fetchUserInfo()

    expect(getUserInfoMock).not.toHaveBeenCalled()
    expect(store.isLoggedIn).toBe(true)
  })

  it('setUser 保存非敏感 hint（用户名 + 过期时间），不写任何令牌', () => {
    const store = useAppStore()
    store.setUser({ username: 'alice', accessTokenExpiresIn: 3600 })

    const hint = readAuthHint()
    expect(hint?.username).toBe('alice')
    expect(hint?.accessTokenExpiresAt).toBeGreaterThan(Date.now())
    expect(localStorage.getItem(LEGACY_ACCESS_TOKEN_STORAGE_KEY)).toBeNull()
    expect(localStorage.getItem(LEGACY_REFRESH_TOKEN_STORAGE_KEY)).toBeNull()
  })

  it('clearUser 清掉 hint 与登录态', () => {
    const store = useAppStore()
    store.setUser(createUserInfo())
    expect(hasAuthHint()).toBe(true)

    store.clearUser()

    expect(hasAuthHint()).toBe(false)
    expect(store.isLoggedIn).toBe(false)
    expect(store.userInfo).toBeNull()
  })

  it('bootstrapAuth 成功后记录新鲜期，随后的 fetchUserInfo 不再请求', async () => {
    seedHint()
    getUserInfoMock.mockResolvedValue({ userInfo: createUserInfo() })
    const store = useAppStore()

    await store.bootstrapAuth()
    expect(getUserInfoMock).toHaveBeenCalledTimes(1)
    expect(store.isLoggedIn).toBe(true)

    await store.fetchUserInfo()
    expect(getUserInfoMock).toHaveBeenCalledTimes(1)
  })

  it('bootstrapAuth 鉴权期间 fetchUserInfo 等待其落定并复用结果', async () => {
    seedHint()
    let release!: (value: { userInfo: UserInfo | null }) => void
    getUserInfoMock.mockImplementation(
      () =>
        new Promise<{ userInfo: UserInfo | null }>((resolve) => {
          release = resolve
        })
    )
    const store = useAppStore()
    const bootstrap = store.bootstrapAuth()
    const pending = store.fetchUserInfo()
    expect(getUserInfoMock).toHaveBeenCalledTimes(1)

    release({ userInfo: createUserInfo() })
    await Promise.all([bootstrap, pending])

    expect(getUserInfoMock).toHaveBeenCalledTimes(1)
    expect(store.isLoggedIn).toBe(true)
  })

  it('force 重查先返回时，迟到的旧响应不覆盖登录态', async () => {
    let releaseOld!: (value: { userInfo: UserInfo | null }) => void
    let releaseForce!: (value: { userInfo: UserInfo | null }) => void
    getUserInfoMock
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            releaseOld = resolve
          })
      )
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            releaseForce = resolve
          })
      )
    const store = useAppStore()

    const oldRequest = store.fetchUserInfo()
    const forceRequest = store.fetchUserInfo({ force: true })

    // 登出核验先回来：服务端已无会话
    releaseForce({ userInfo: null })
    await forceRequest
    expect(store.isLoggedIn).toBe(false)

    // 旧请求迟到，带着登出前的用户信息：不能把登录态又翻回 true
    releaseOld({ userInfo: createUserInfo() })
    await oldRequest
    expect(store.isLoggedIn).toBe(false)
  })

  it('bootstrapAuth 的迟到响应不覆盖 force 核验出的未登录态', async () => {
    seedHint()
    let releaseBootstrap!: (value: { userInfo: UserInfo | null }) => void
    let releaseForce!: (value: { userInfo: UserInfo | null }) => void
    getUserInfoMock
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            releaseBootstrap = resolve
          })
      )
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            releaseForce = resolve
          })
      )
    const store = useAppStore()

    const bootstrap = store.bootstrapAuth()
    const force = store.fetchUserInfo({ force: true })

    releaseForce({ userInfo: null })
    await force
    expect(store.isLoggedIn).toBe(false)

    // bootstrap 的响应迟到：序号已过期，不能再写回登录态
    releaseBootstrap({ userInfo: createUserInfo() })
    await bootstrap
    expect(store.isLoggedIn).toBe(false)
  })
})

describe('app store 启动恢复（Cookie 会话）', () => {
  it('没有 hint 视为未登录，不发任何请求', async () => {
    const store = useAppStore()

    await store.bootstrapAuth()

    expect(getUserInfoMock).not.toHaveBeenCalled()
    expect(refreshMock).not.toHaveBeenCalled()
    expect(store.isLoggedIn).toBe(false)
  })

  it('Cookie 有效：getUserInfo 直接恢复登录态，不需要刷新', async () => {
    seedHint()
    getUserInfoMock.mockResolvedValue({ userInfo: createUserInfo() })
    const store = useAppStore()

    await store.bootstrapAuth()

    expect(getUserInfoMock).toHaveBeenCalledTimes(1)
    expect(refreshMock).not.toHaveBeenCalled()
    expect(store.isLoggedIn).toBe(true)
  })

  it('getUserInfo 返回 null 且 hint 存在：刷新一次再取，取到即登录', async () => {
    seedHint()
    // 模拟真实刷新成功会更新 hint 过期时间
    refreshMock.mockImplementation(async () => {
      storeAuthHint({ username: 'admin', accessTokenExpiresAt: Date.now() + 6 * 3600 * 1000 })
    })
    getUserInfoMock
      .mockResolvedValueOnce({ userInfo: null })
      .mockResolvedValueOnce({ userInfo: createUserInfo() })
    const store = useAppStore()

    await store.bootstrapAuth()

    expect(refreshMock).toHaveBeenCalledTimes(1)
    expect(getUserInfoMock).toHaveBeenCalledTimes(2)
    expect(store.isLoggedIn).toBe(true)
  })

  it('刷新被服务端拒绝：视为未登录', async () => {
    seedHint()
    const { SessionExpiredError } = await import('../utils/authSession')
    getUserInfoMock.mockResolvedValue({ userInfo: null })
    refreshMock.mockRejectedValue(new SessionExpiredError())
    const store = useAppStore()

    await store.bootstrapAuth()

    expect(store.isLoggedIn).toBe(false)
    expect(store.userInfo).toBeNull()
  })

  it('网络失败且 hint 存在：保持登录态用于离线，hint 不丢', async () => {
    seedHint()
    getUserInfoMock.mockRejectedValue(new Error('offline'))
    refreshMock.mockRejectedValue(new Error('offline'))
    const store = useAppStore()

    await store.bootstrapAuth()

    expect(store.isLoggedIn).toBe(true)
    expect(hasAuthHint()).toBe(true)
    expect(readAuthHint()?.username).toBe('admin')
  })

  it('旧 localStorage 令牌迁移：先带旧 Refresh Token 刷新，成功后删除旧键', async () => {
    seedLegacyTokens()
    // 模拟真实刷新成功：换到 Cookie 会话并留下 hint
    refreshMock.mockImplementation(async () => {
      storeAuthHint({ username: '', accessTokenExpiresAt: Date.now() + 6 * 3600 * 1000 })
    })
    getUserInfoMock.mockResolvedValue({ userInfo: createUserInfo() })
    const store = useAppStore()

    await store.bootstrapAuth()

    expect(refreshMock).toHaveBeenCalledTimes(1)
    expect(localStorage.getItem(LEGACY_ACCESS_TOKEN_STORAGE_KEY)).toBeNull()
    expect(localStorage.getItem(LEGACY_REFRESH_TOKEN_STORAGE_KEY)).toBeNull()
    expect(localStorage.getItem(LEGACY_ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY)).toBeNull()
    expect(store.isLoggedIn).toBe(true)
    // hint 的用户名由随后的 getUserInfo 补齐
    expect(readAuthHint()?.username).toBe('admin')
  })

  it('旧令牌迁移失败也删除旧键，且不残留登录态', async () => {
    seedLegacyTokens()
    const { SessionExpiredError } = await import('../utils/authSession')
    refreshMock.mockRejectedValue(new SessionExpiredError())
    const store = useAppStore()

    await store.bootstrapAuth()

    expect(localStorage.getItem(LEGACY_REFRESH_TOKEN_STORAGE_KEY)).toBeNull()
    expect(store.isLoggedIn).toBe(false)
    expect(hasAuthHint()).toBe(false)
  })
})

describe('app store 会话失效清理', () => {
  it('updateUserInfo 只在 hint 已存在时补用户名，不会为失效会话新造 hint', async () => {
    const store = useAppStore()
    clearAuthHint()

    store.updateUserInfo(createUserInfo('late-user'))

    expect(store.isLoggedIn).toBe(true)
    expect(hasAuthHint()).toBe(false)
  })
})
