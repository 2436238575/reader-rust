import { defineStore } from 'pinia'
import { ref, watch, computed } from 'vue'
import { getUserInfo } from '../api/user'
import type { UserInfo } from '../types'
import { applySystemTheme } from '../utils/systemUi'
import { ACCESS_TOKEN_STORAGE_KEY } from '../utils/secureAccess'

export const useAppStore = defineStore('app', () => {
  const STATS_KEY = 'reader-stats'
  // ─── Theme ───
  const savedTheme = localStorage.getItem('theme') as 'light' | 'dark' | null
  const legacyReaderNight = localStorage.getItem('reader-isNight') === 'true'
  const theme = ref<'light' | 'dark'>(
    savedTheme || (legacyReaderNight ? 'dark' : 'light')
  )

  function setTheme(value: 'light' | 'dark') {
    theme.value = value
    localStorage.setItem('theme', value)
    applySystemTheme(value)
  }

  function toggleTheme() {
    setTheme(theme.value === 'light' ? 'dark' : 'light')
  }

  watch(theme, (val) => {
    localStorage.setItem('theme', val)
    applySystemTheme(val)
  }, { immediate: true })

  // ─── User ───
  const userInfo = ref<UserInfo | null>(null)
  const isLoggedIn = ref(false)

  async function fetchUserInfo() {
    try {
      const data = await getUserInfo()
      userInfo.value = data.userInfo
      isLoggedIn.value = !!data.userInfo?.username
    } catch {
      isLoggedIn.value = false
    }
  }

  function setUser(user: UserInfo) {
    userInfo.value = user
    isLoggedIn.value = true
    if (user.accessToken) {
      localStorage.setItem(ACCESS_TOKEN_STORAGE_KEY, user.accessToken)
    }
  }

  /**
   * 换发令牌后更新本地副本。
   *
   * 改密码会让服务端自增撤销版本号、作废其他设备上的令牌，同时为当前设备
   * 换发新令牌；不保存它的话当前设备也会被登出。
   */
  function setAccessToken(token: string) {
    if (token) {
      localStorage.setItem(ACCESS_TOKEN_STORAGE_KEY, token)
    }
  }

  function clearUser() {
    userInfo.value = null
    isLoggedIn.value = false
    localStorage.removeItem(ACCESS_TOKEN_STORAGE_KEY)
  }

  function updateUserInfo(next: UserInfo | null) {
    userInfo.value = next
    isLoggedIn.value = !!next?.username
  }

  // ─── UI State ───
  const showSettingsDrawer = ref(false)
  const showSourceManager = ref(false)
  const showWebdavManager = ref(false)
  const showCacheLibrary = ref(false)
  const isOnline = ref(typeof navigator !== 'undefined' ? navigator.onLine : true)
  const pwaReady = ref(false)
  const pwaUpdateAvailable = ref(false)
  const deferredInstallPrompt = ref<any>(null)
  const waitingServiceWorker = ref<ServiceWorker | null>(null)

  const initialReadingStats = (() => {
    try {
      return JSON.parse(localStorage.getItem(STATS_KEY) || '{"totalSeconds":0,"openedBooks":[],"readChapters":[],"completedBooks":[]}')
    } catch {
      return { totalSeconds: 0, openedBooks: [], readChapters: [], completedBooks: [] }
    }
  })()

  const readingStats = ref<{
    totalSeconds: number
    openedBooks: string[]
    readChapters: string[]
    completedBooks: string[]
  }>(initialReadingStats)
  let readingSessionStartedAt = 0

  function persistStats() {
    localStorage.setItem(STATS_KEY, JSON.stringify(readingStats.value))
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

  // readChapters 的保留条数上限（只作累计统计用，裁掉最旧的）
  const MAX_READ_CHAPTER_ENTRIES = 5000

  function markChapterRead(bookUrl: string, index: number, totalChapters: number) {
    const key = `${bookUrl}#${index}`
    if (!readingStats.value.readChapters.includes(key)) {
      readingStats.value.readChapters.push(key)
      // 无上限会随使用年限膨胀到数万条且每章全量 stringify；只统计总数，裁掉最旧的
      if (readingStats.value.readChapters.length > MAX_READ_CHAPTER_ENTRIES) {
        readingStats.value.readChapters.splice(0, readingStats.value.readChapters.length - MAX_READ_CHAPTER_ENTRIES)
      }
    }
    if (totalChapters > 0 && index >= totalChapters - 1 && !readingStats.value.completedBooks.includes(bookUrl)) {
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

  function showToast(message: string, type: 'success' | 'error' | 'warning' = 'success') {
    const id = ++toastId
    toasts.value.push({ id, message, type })
    setTimeout(() => {
      toasts.value = toasts.value.filter((t) => t.id !== id)
    }, 3000)
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
    theme, setTheme, toggleTheme,
    userInfo, isLoggedIn,
    fetchUserInfo, setUser, setAccessToken, clearUser, updateUserInfo,
    showSettingsDrawer, showSourceManager, showWebdavManager, showCacheLibrary,
    isOnline, pwaReady, pwaUpdateAvailable, deferredInstallPrompt, waitingServiceWorker,
    setOnlineStatus, setPwaReady, setPwaUpdateAvailable, setDeferredInstallPrompt, setWaitingServiceWorker, installPwa, applyPwaUpdate,
    readingStats, readingStatsSummary, startReadingSession, stopReadingSession, markBookOpened, markChapterRead,
    toasts, showToast,
  }
})
