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
import { invalidateSessionForRetry } from '../utils/sessionEpoch'
import { RECENT_BOOKS } from '../utils/userdataDocs'
import { getReadingStats, type ReadingStats } from '../api/readStats'
import {
  accumulateReadSeconds,
  flushReadEvents,
  flushReadEventsKeepalive,
  syncReadDayKey,
  trackChapterRead,
} from '../utils/readEvents'

function formatReadingDuration(seconds: number) {
  const safe = Math.max(0, Math.floor(seconds))
  if (safe <= 0) return '0分钟'
  const totalMinutes = Math.floor(safe / 60)
  if (totalMinutes < 1) return `${safe}秒`
  const hours = Math.floor(totalMinutes / 60)
  const minutes = totalMinutes % 60
  return hours ? `${hours}小时${minutes}分钟` : `${minutes}分钟`
}

export const useAppStore = defineStore('app', () => {
  // ─── Theme ───
  const savedTheme = localStorage.getItem('theme') as 'light' | 'dark' | null
  const theme = ref<'light' | 'dark'>(savedTheme || 'light')

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

  // 最近阅读仍走 userdata；阅读统计/已读标记已改由服务端派生，不再预读
  function prefetchFirstScreenDocs() {
    void prefetchUserdata([RECENT_BOOKS])
  }

  /** 拉取服务端派生统计；失败保留上一次结果（统计页不是关键路径） */
  async function refreshReadingStats() {
    if (!isLoggedIn.value) {
      serverReadingStats.value = null
      return
    }
    try {
      serverReadingStats.value = await getReadingStats()
    } catch {
      /* 离线/临时失败：保留上次快照 */
    }
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
      void refreshReadingStats()
    })().finally(() => {
      if (userInfoInFlight === request) userInfoInFlight = null
    })
    userInfoInFlight = request
    return request
  }

  /** 登录/改密响应缺省时按后端默认的 6 小时计。 */
  const DEFAULT_ACCESS_TOKEN_TTL_SECS = 6 * 3600

  /** /v1 登录/改密返回的会话信息（2026-11 起令牌过期时间由服务端给 RFC3339）。 */
  interface SessionProfile {
    username: string
    accessTokenExpiresAt: string
  }

  function setUser(user: SessionProfile) {
    userInfo.value = { username: user.username } as UserInfo
    isLoggedIn.value = true
    // 登录态刚由服务端确认：登录后 HomeView 立刻调用的 fetchUserInfo
    // 直接复用，不再发一次 getUserInfo
    markUserInfoFresh()
    // 凭据在 HttpOnly Cookie 里，前端只留非敏感 hint（用户名 + 过期时间）；
    // 响应缺省/解析失败时按默认 TTL 兜底
    const expiresAtMs = Date.parse(user.accessTokenExpiresAt)
    storeAuthHint({
      username: user.username,
      accessTokenExpiresAt: Number.isFinite(expiresAtMs)
        ? expiresAtMs
        : Date.now() + DEFAULT_ACCESS_TOKEN_TTL_SECS * 1000,
    })
    startAuthLifecycle()
    // 登录后不一定经过会调 fetchUserInfo 的页面，直接在登录点触发
    void refreshReadingStats()
    prefetchFirstScreenDocs()
  }

  /**
   * 改密成功（204，无响应体）：后端换发了当前会话 Cookie，hint 只需重排
   * 本地过期时间（用户名沿用现值）。
   */
  function refreshSessionHint(ttlSecs = DEFAULT_ACCESS_TOKEN_TTL_SECS) {
    const username =
      userInfo.value?.username ?? readAuthHint()?.username ?? ''
    storeAuthHint({
      username,
      accessTokenExpiresAt: Date.now() + ttlSecs * 1000,
    })
    startAuthLifecycle()
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
          void refreshReadingStats()
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
          void refreshReadingStats()
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
    // 登出/会话失效：在途请求的 401 不再触发静默刷新
    invalidateSessionForRetry()
    userInfo.value = null
    isLoggedIn.value = false
    serverReadingStats.value = null
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

  const serverReadingStats = ref<ReadingStats | null>(null)
  const STATS_TICK_MS = 15_000
  let readingSessionStartedAt = 0
  let lastReadingTickAt = 0
  let readingStatsTimer: ReturnType<typeof setInterval> | null = null
  let readingSessionPaused = false
  let activeChapter: { bookId: string; chapterIndex: number } | null = null

  /** 把上次 tick 以来的秒数记到当前章；切章/暂停/落页时都会先调一次。 */
  function accrueActiveSeconds() {
    if (!readingSessionStartedAt || readingSessionPaused) {
      lastReadingTickAt = Date.now()
      return
    }
    const now = Date.now()
    if (!activeChapter) {
      lastReadingTickAt = now
      return
    }
    const elapsedSeconds = Math.floor((now - lastReadingTickAt) / 1000)
    if (elapsedSeconds <= 0) return
    accumulateReadSeconds(
      activeChapter.bookId,
      activeChapter.chapterIndex,
      elapsedSeconds
    )
    lastReadingTickAt += elapsedSeconds * 1000
  }

  function tickReadingSession() {
    if (readingSessionPaused) return
    accrueActiveSeconds()
    if (activeChapter) void flushReadEvents().catch(() => undefined)
  }

  function startReadingSession() {
    if (readingSessionStartedAt) return
    readingSessionStartedAt = Date.now()
    lastReadingTickAt = readingSessionStartedAt
    readingSessionPaused = false
    // 先把上一会话/跨日遗留的累计值按它所属的 dayKey 发出去，再校准当前日；
    // 服务端会把这些事件写回旧 dayKey，不会污染今天。
    void flushReadEvents()
      .then(() => syncReadDayKey())
      .catch(() => undefined)
    if (readingStatsTimer) clearInterval(readingStatsTimer)
    readingStatsTimer = setInterval(tickReadingSession, STATS_TICK_MS)
  }

  /** 页面隐藏/切后台时暂停计时，避免把挂机时间算成阅读。 */
  function pauseReadingSession() {
    if (!readingSessionStartedAt || readingSessionPaused) return
    accrueActiveSeconds()
    readingSessionPaused = true
  }

  function resumeReadingSession() {
    if (!readingSessionStartedAt || !readingSessionPaused) return
    readingSessionPaused = false
    lastReadingTickAt = Date.now()
  }

  function stopReadingSession() {
    if (!readingSessionStartedAt) return
    tickReadingSession()
    readingSessionStartedAt = 0
    readingSessionPaused = false
    activeChapter = null
    if (readingStatsTimer) {
      clearInterval(readingStatsTimer)
      readingStatsTimer = null
    }
    void flushReadEvents().catch(() => undefined)
  }

  /** pagehide/beforeunload 兜底：同步记完最后一段秒数并 keepalive 上报。 */
  function flushReadingSessionKeepalive() {
    accrueActiveSeconds()
    flushReadEventsKeepalive()
  }

  /** 翻章时调用：先把上一章剩余秒数记上，再切到新章并登记已读。 */
  function markChapterRead(bookId: string, index: number) {
    if (!bookId || !Number.isInteger(index) || index < 0) return
    accrueActiveSeconds()
    activeChapter = { bookId, chapterIndex: index }
    lastReadingTickAt = Date.now()
    trackChapterRead(bookId, index)
    void flushReadEvents().catch(() => undefined)
  }

  const readingStatsSummary = computed(() => {
    const stats = serverReadingStats.value
    return {
      totalSeconds: stats?.totalSeconds ?? 0,
      todaySeconds: stats?.todaySeconds ?? 0,
      totalTimeText: formatReadingDuration(stats?.totalSeconds ?? 0),
      todayTimeText: formatReadingDuration(stats?.todaySeconds ?? 0),
      readChapters: stats?.readChapters ?? 0,
      finishedBooks: stats?.finishedBooks ?? 0,
      streakDays: stats?.streakDays ?? 0,
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
    // 恢复联网后先把离线缓冲按它所属的 dayKey 发出，再校准当前日；
    // 反序会让跨日缓冲被当成旧数据清掉。
    if (value) {
      void flushReadEvents()
        .then(() => syncReadDayKey())
        .catch(() => undefined)
    }
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
    refreshSessionHint,
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
    serverReadingStats,
    readingStatsSummary,
    refreshReadingStats,
    startReadingSession,
    stopReadingSession,
    pauseReadingSession,
    resumeReadingSession,
    flushReadingSessionKeepalive,
    markChapterRead,
    toasts,
    showToast,
  }
})
