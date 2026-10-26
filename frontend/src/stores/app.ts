import { defineStore } from 'pinia'
import { ref, watch, computed } from 'vue'
import { getUserInfo } from '../api/user'
import type { UserInfo } from '../types'
import { applySystemTheme } from '../utils/systemUi'
import {
  clearAuthHint,
  hasAuthHint,
  readAuthHint,
  storeAuthHint,
} from '../utils/secureAccess'
import {
  SessionExpiredError,
  refreshAccessToken,
  startAuthLifecycle,
  stopAuthLifecycle,
} from '../utils/authSession'
import { cancelUserdataQueue } from '../utils/userdataQueue'
import { clearUserdataPrefetch, prefetchUserdata } from '../api/userdata'
import { READ_HISTORY, READING_STATS, RECENT_BOOKS } from '../utils/userdataDocs'
import {
  loadLocalReadingStats,
  persistLocalReadingStats,
  pushReadingStatsToBackend,
  syncReadingStatsFromBackend,
  MAX_READ_CHAPTER_ENTRIES,
  STATS_PUSH_DEBOUNCE_MS,
  type ReadingStats,
} from '../utils/readingStats'

export const useAppStore = defineStore('app', () => {
  // ─── Theme ───
  const savedTheme = localStorage.getItem('theme') as 'light' | 'dark' | null
  const legacyReaderNight = localStorage.getItem('reader-isNight') === 'true'
  const theme = ref<'light' | 'dark'>(savedTheme || (legacyReaderNight ? 'dark' : 'light'))

  function setTheme(value: 'light' | 'dark') {
    theme.value = value
    localStorage.setItem('theme', value)
    applySystemTheme(value)
  }

  function toggleTheme() {
    setTheme(theme.value === 'light' ? 'dark' : 'light')
  }

  watch(
    theme,
    (val) => {
      localStorage.setItem('theme', val)
      applySystemTheme(val)
    },
    { immediate: true }
  )

  // ─── User ───
  const userInfo = ref<UserInfo | null>(null)
  const isLoggedIn = ref(false)

  // getUserInfo 的 60 秒成功新鲜期与进行中请求：App bootstrap（bootstrapAuth）、
  // HomeView / AiBookView 的 onMounted 会重复调用 fetchUserInfo，实测冷启动 3 连发。
  // 新鲜期内直接复用内存态，进行中的合并复用同一个请求；网络失败不记新鲜时间，
  // 下一次调用立即重试。need-login、登出后的核验走 force 绕过。
  const USER_INFO_FRESH_MS = 60_000
  let userInfoFreshAt = 0
  let userInfoInFlight: Promise<void> | null = null
  // 每发起一次请求自增：force 重查可能赶在旧请求前面返回，旧响应迟到时
  // 不能再把刚拿到的登录态覆盖回去（登出核验尤其要紧）
  let userInfoSeq = 0

  /** 拿到服务端返回的用户信息时记录新鲜时间（fetchUserInfo / bootstrapAuth / setUser）。 */
  function markUserInfoFresh() {
    userInfoFreshAt = Date.now()
  }

  // 每个会话只做一次后端合并；登出后复位
  let statsSynced = false

  // 首屏三份 userdata 文档（阅读统计/最近阅读/已读标记）一次批量预读，
  // 随后的单文档读取命中 30s 一次性缓存，省三次往返；失败静默由单文档 GET 兜底
  function prefetchFirstScreenDocs() {
    void prefetchUserdata([READING_STATS, RECENT_BOOKS, READ_HISTORY])
  }

  /** 登录态就绪后从后端合并一次阅读统计（跨浏览器同步）；失败则继续用本地 */
  function syncStatsOnce() {
    if (!isLoggedIn.value || statsSynced) return
    statsSynced = true
    void syncReadingStatsFromBackend(readingStats.value)
      .then((merged) => {
        readingStats.value = merged
      })
      .catch(() => undefined)
  }

  async function fetchUserInfo(options: { force?: boolean } = {}) {
    if (!options.force) {
      // 启动期 bootstrapAuth 正在鉴权时等它落定：成功会记录新鲜期，
      // 落定后走下面的新鲜期检查，冷启动就只发一次 getUserInfo
      if (bootstrapPromise) await bootstrapPromise.catch(() => undefined)
      if (userInfoFreshAt && Date.now() - userInfoFreshAt < USER_INFO_FRESH_MS) return
      if (userInfoInFlight) return userInfoInFlight
    }
    // request 是含 finally 的完整链：finally 回调里比较的就是存进映射的同一个
    // promise，落定后指针才能被正确清除（拆开存会导致身份不等、指针永不清除）
    const seq = ++userInfoSeq
    const request = (async () => {
      try {
        const data = await getUserInfo()
        if (seq !== userInfoSeq) return
        userInfo.value = data.userInfo
        isLoggedIn.value = !!data.userInfo?.username
        // 只有真的拿到响应才记新鲜时间；网络失败不记，方便马上重试
        markUserInfoFresh()
      } catch {
        if (seq !== userInfoSeq) return
        // 网络失败：有登录提示时保留登录态用于离线（真正的失效由 401 与
        // 刷新失败处理）；没有提示才按未登录处理
        if (!hasAuthHint()) isLoggedIn.value = false
      }
      syncStatsOnce()
    })().finally(() => {
      if (userInfoInFlight === request) userInfoInFlight = null
    })
    userInfoInFlight = request
    return request
  }

  /** 登录/改密响应缺省时按后端默认的 6 小时计。 */
  const DEFAULT_ACCESS_TOKEN_TTL_SECS = 6 * 3600

  function setUser(user: UserInfo) {
    userInfo.value = user
    isLoggedIn.value = true
    // 服务端登录响应带来的用户信息是新鲜的：登录后 HomeView 立刻调用的
    // fetchUserInfo 直接复用，不再发一次 getUserInfo
    markUserInfoFresh()
    // 凭据在 HttpOnly Cookie 里，前端只留非敏感 hint（用户名 + 过期时间）
    storeAuthHint({
      username: user.username,
      accessTokenExpiresAt:
        Date.now() + (user.accessTokenExpiresIn ?? DEFAULT_ACCESS_TOKEN_TTL_SECS) * 1000,
    })
    startAuthLifecycle()
    // 登录后不一定经过会调 fetchUserInfo 的页面，直接在登录点触发
    syncStatsOnce()
    prefetchFirstScreenDocs()
  }

  /** 服务端确认的用户信息落地：hint 已存在时补齐用户名（首刷时可能未知）。 */
  function syncHintUsername(username: string) {
    const hint = readAuthHint()
    // 不新造 hint：会话刚失效（hint 已清）时不能把登录提示又造回来
    if (hint) {
      storeAuthHint({ username, accessTokenExpiresAt: hint.accessTokenExpiresAt })
    }
  }

  /**
   * 启动时恢复会话。会话凭据在 HttpOnly Cookie 里，浏览器自动携带：
   * 1. 没有 hint 视为未登录（真正的会话有效性由 API 401 兜底）；
   * 2. getUserInfo 是可选鉴权端点：Cookie 过期时它返回 200 + null，此时
   *    刷新一次再取；
   * 3. 网络故障且 hint 存在时保持登录态（离线打开仍能进阅读器），等
   *    后续请求再试。
   */
  let bootstrapPromise: Promise<void> | null = null
  function bootstrapAuth(): Promise<void> {
    if (bootstrapPromise) return bootstrapPromise
    bootstrapPromise = (async () => {
      if (!hasAuthHint()) {
        isLoggedIn.value = false
        return
      }

      // 与 fetchUserInfo 共用序号：force 重查可能在这两次请求在途时发生，
      // 迟到的 bootstrap 响应不能再把刚核验出的登录态写回去
      const bootstrapSeq = ++userInfoSeq
      try {
        const data = await getUserInfo()
        if (bootstrapSeq !== userInfoSeq) return
        if (data.userInfo?.username) {
          userInfo.value = data.userInfo
          isLoggedIn.value = true
          syncHintUsername(data.userInfo.username)
          // 这次响应就是权威登录态：随后的 fetchUserInfo 复用，不再重复请求
          markUserInfoFresh()
          startAuthLifecycle()
          syncStatsOnce()
          prefetchFirstScreenDocs()
          return
        }
      } catch {
        // 网络故障：继续尝试刷新；刷新也失败则按离线登录态处理
      }

      let seq = bootstrapSeq
      try {
        await refreshAccessToken()
        seq = ++userInfoSeq
        const data = await getUserInfo()
        if (seq !== userInfoSeq) return
        userInfo.value = data.userInfo
        isLoggedIn.value = !!data.userInfo?.username
        if (isLoggedIn.value) {
          syncHintUsername(data.userInfo!.username)
          markUserInfoFresh()
          startAuthLifecycle()
          syncStatsOnce()
          prefetchFirstScreenDocs()
        } else {
          // Cookie 刚换新仍取不到用户：会话确实没了
          clearAuthHint()
          stopAuthLifecycle()
        }
      } catch (error) {
        if (seq !== userInfoSeq) return
        if (error instanceof SessionExpiredError) {
          userInfo.value = null
          isLoggedIn.value = false
          return
        }
        // 网络错误：hint 保留，按离线登录态放行（路由守卫依赖 hint），
        // 下一次请求会再次触发刷新
        if (hasAuthHint()) isLoggedIn.value = true
      }
    })().finally(() => {
      bootstrapPromise = null
    })
    return bootstrapPromise
  }

  function clearUser() {
    userInfo.value = null
    isLoggedIn.value = false
    statsSynced = false
    clearAuthHint()
    stopAuthLifecycle()
    // 会话 Cookie 已失效：取消 userdata 待写队列，否则残留的退避重试会
    // 401 触发 need-login，把刚登出的用户强制弹去登录页；待写文档本地有镜像，
    // 下次登录的同步合并会推回，丢弃不丢数据
    cancelUserdataQueue()
    // 预读缓存属于上一个会话，一并清掉
    clearUserdataPrefetch()
  }

  function updateUserInfo(next: UserInfo | null) {
    userInfo.value = next
    isLoggedIn.value = !!next?.username
    // 只有写入了服务端返回的真实用户信息才算新鲜；置空/登出不记
    if (next?.username) {
      markUserInfoFresh()
      syncHintUsername(next.username)
    }
  }

  // ─── UI State ───
  const showSettingsDrawer = ref(false)
  const showCacheLibrary = ref(false)
  const showAiSettings = ref(false)
  const isOnline = ref(typeof navigator !== 'undefined' ? navigator.onLine : true)
  const pwaReady = ref(false)
  const pwaUpdateAvailable = ref(false)
  const deferredInstallPrompt = ref<any>(null)
  const waitingServiceWorker = ref<ServiceWorker | null>(null)

  const readingStats = ref<ReadingStats>(loadLocalReadingStats())
  let readingSessionStartedAt = 0
  let statsPushTimer: ReturnType<typeof setTimeout> | null = null

  function persistStats() {
    persistLocalReadingStats(readingStats.value)
    // 防抖推后端：快速连读多章时合并为一次写入
    if (statsPushTimer) clearTimeout(statsPushTimer)
    statsPushTimer = setTimeout(() => {
      statsPushTimer = null
      pushReadingStatsToBackend(readingStats.value)
    }, STATS_PUSH_DEBOUNCE_MS)
  }

  function startReadingSession() {
    if (!readingSessionStartedAt) readingSessionStartedAt = Date.now()
  }

  function stopReadingSession() {
    if (!readingSessionStartedAt) return
    const delta = Math.max(0, Math.round((Date.now() - readingSessionStartedAt) / 1000))
    readingStats.value.totalSeconds += delta
    readingSessionStartedAt = 0
    persistStats()
  }

  function markBookOpened(bookUrl: string) {
    if (!readingStats.value.openedBooks.includes(bookUrl)) {
      readingStats.value.openedBooks.push(bookUrl)
      persistStats()
    }
  }

  function markChapterRead(bookUrl: string, index: number, totalChapters: number) {
    const key = `${bookUrl}#${index}`
    if (!readingStats.value.readChapters.includes(key)) {
      readingStats.value.readChapters.push(key)
      // 无上限会随使用年限膨胀到数万条且每章全量 stringify；只统计总数，裁掉最旧的
      if (readingStats.value.readChapters.length > MAX_READ_CHAPTER_ENTRIES) {
        readingStats.value.readChapters.splice(
          0,
          readingStats.value.readChapters.length - MAX_READ_CHAPTER_ENTRIES
        )
      }
    }
    if (
      totalChapters > 0 &&
      index >= totalChapters - 1 &&
      !readingStats.value.completedBooks.includes(bookUrl)
    ) {
      readingStats.value.completedBooks.push(bookUrl)
    }
    persistStats()
  }
  const readingStatsSummary = computed(() => {
    const totalMinutes = Math.floor(readingStats.value.totalSeconds / 60)
    const hours = Math.floor(totalMinutes / 60)
    const minutes = totalMinutes % 60
    return {
      totalSeconds: readingStats.value.totalSeconds,
      totalTimeText: hours ? `${hours}小时${minutes}分钟` : `${Math.max(1, totalMinutes)}分钟`,
      openedBooks: readingStats.value.openedBooks.length,
      readChapters: readingStats.value.readChapters.length,
      completedBooks: readingStats.value.completedBooks.length,
    }
  })

  // ─── Toast ───
  const toasts = ref<Array<{ id: number; message: string; type: string }>>([])
  let toastId = 0

  /** 按类型分时长：错误文案通常较长需要读完，成功提示尽量不打断 */
  const TOAST_DURATION_MS: Record<'success' | 'warning' | 'error', number> = {
    success: 3000,
    warning: 4500,
    error: 6000,
  }

  function showToast(message: string, type: 'success' | 'error' | 'warning' = 'success') {
    const id = ++toastId
    toasts.value.push({ id, message, type })
    setTimeout(() => {
      toasts.value = toasts.value.filter((t) => t.id !== id)
    }, TOAST_DURATION_MS[type])
  }

  function setOnlineStatus(value: boolean) {
    isOnline.value = value
  }

  function setPwaReady(value: boolean) {
    pwaReady.value = value
  }

  function setPwaUpdateAvailable(value: boolean) {
    pwaUpdateAvailable.value = value
  }

  function setWaitingServiceWorker(value: ServiceWorker | null) {
    waitingServiceWorker.value = value
  }

  function setDeferredInstallPrompt(value: any) {
    deferredInstallPrompt.value = value
  }

  async function installPwa() {
    if (!deferredInstallPrompt.value) return false
    deferredInstallPrompt.value.prompt()
    const result = await deferredInstallPrompt.value.userChoice.catch(() => null)
    deferredInstallPrompt.value = null
    return result?.outcome === 'accepted'
  }

  function applyPwaUpdate() {
    if (!waitingServiceWorker.value) return false
    waitingServiceWorker.value.postMessage({ type: 'SKIP_WAITING' })
    return true
  }

  return {
    theme,
    setTheme,
    toggleTheme,
    userInfo,
    isLoggedIn,
    fetchUserInfo,
    bootstrapAuth,
    setUser,
    clearUser,
    updateUserInfo,
    showSettingsDrawer,
    showCacheLibrary,
    showAiSettings,
    isOnline,
    pwaReady,
    pwaUpdateAvailable,
    deferredInstallPrompt,
    waitingServiceWorker,
    setOnlineStatus,
    setPwaReady,
    setPwaUpdateAvailable,
    setDeferredInstallPrompt,
    setWaitingServiceWorker,
    installPwa,
    applyPwaUpdate,
    readingStats,
    readingStatsSummary,
    startReadingSession,
    stopReadingSession,
    markBookOpened,
    markChapterRead,
    toasts,
    showToast,
  }
})
