// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useAppStore } from './app'
import { getUserInfo } from '../api/user'
import { ACCESS_TOKEN_STORAGE_KEY } from '../utils/secureAccess'
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

// startAuthLifecycle 会挂定时器，单测不需要真实的续期编排
vi.mock('../utils/authSession', () => ({
  SessionExpiredError: class SessionExpiredError extends Error {
    constructor() {
      super('登录状态已失效')
      this.name = 'SessionExpiredError'
    }
  },
  refreshAccessToken: vi.fn(),
  startAuthLifecycle: vi.fn(),
  stopAuthLifecycle: vi.fn(),
}))

vi.mock('../utils/systemUi', () => ({
  applySystemTheme: vi.fn(),
}))

const getUserInfoMock = vi.mocked(getUserInfo)

function createUserInfo(username = 'admin'): UserInfo {
  return { username }
}

describe('app store fetchUserInfo 合并与新鲜期', () => {
  beforeEach(() => {
    localStorage.clear()
    setActivePinia(createPinia())
    getUserInfoMock.mockReset()
  })

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

  it('setUser 写入服务端用户信息后，fetchUserInfo 直接复用不再请求', async () => {
    const store = useAppStore()
    store.setUser(createUserInfo())

    await store.fetchUserInfo()

    expect(getUserInfoMock).not.toHaveBeenCalled()
    expect(store.isLoggedIn).toBe(true)
  })

  it('bootstrapAuth 成功后记录新鲜期，随后的 fetchUserInfo 不再请求', async () => {
    localStorage.setItem(ACCESS_TOKEN_STORAGE_KEY, 'token')
    getUserInfoMock.mockResolvedValue({ userInfo: createUserInfo() })
    const store = useAppStore()

    await store.bootstrapAuth()
    expect(getUserInfoMock).toHaveBeenCalledTimes(1)
    expect(store.isLoggedIn).toBe(true)

    await store.fetchUserInfo()
    expect(getUserInfoMock).toHaveBeenCalledTimes(1)
  })

  it('bootstrapAuth 鉴权期间 fetchUserInfo 等待其落定并复用结果', async () => {
    localStorage.setItem(ACCESS_TOKEN_STORAGE_KEY, 'token')
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
    localStorage.setItem(ACCESS_TOKEN_STORAGE_KEY, 'token')
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
