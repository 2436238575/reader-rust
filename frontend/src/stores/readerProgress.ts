// 阅读进度的本地会话与服务端上报：从 reader store 拆出的子模块（拆分专项 Phase 4）。
// 工厂形态而非 composable：不碰 Vue 生命周期，store 在 setup 里创建并摊开。
//
// 不在这里的：`restorePersistedSession`（会话恢复要驱动章节加载管线——
// fetchChapterContent/setActiveChapterState/已读/评论全在 store 里，
// 搬走要用 8 个回调把结挪个地方而已），它留在 reader.ts。

import { ref, type Ref } from 'vue'
import { API_BASE } from '../utils/appBase'
import { buildAuthHeaders } from '../utils/secureAccess'
import { safeLocalSet } from '../utils/storage'
import { saveBookProgress } from '../api/bookshelf'
import type { Book, BookChapter } from '../types'
import type { useBookshelfStore } from './bookshelf'

export const READER_SESSION_KEY = 'reader-last-session'
const SERVER_PROGRESS_SCALE = 10000

export interface PersistedReaderSession {
  book: Book
  chapters: BookChapter[]
  currentIndex: number
  chapterScrollProgress: number
  updatedAt: number
}

export interface ReaderProgressDeps {
  book: Ref<Book | null>
  chapters: Ref<BookChapter[]>
  currentIndex: Ref<number>
  chapterScrollProgress: Ref<number>
  shelfStore: ReturnType<typeof useBookshelfStore>
}

export function createReaderProgress(deps: ReaderProgressDeps) {
  const { book, chapters, currentIndex, chapterScrollProgress, shelfStore } = deps

  /// 打开书时捕获的服务端进度时间戳：本地 vs 服务端新旧比较用它
  /// （durChapterTime 在打开章节时会被刷成本机时间，不能做比较基准）
  const serverProgressTime = ref(0)
  const progressDirty = ref(false)
  const lastServerProgressKey = ref('')

  function resetProgressTracking() {
    serverProgressTime.value = 0
    progressDirty.value = false
    lastServerProgressKey.value = ''
  }

  function encodeServerProgress(progress = chapterScrollProgress.value) {
    return Math.max(
      0,
      Math.min(
        SERVER_PROGRESS_SCALE,
        Math.round(Math.max(0, Math.min(1, progress)) * SERVER_PROGRESS_SCALE)
      )
    )
  }

  function decodeServerProgress(position?: number | null) {
    if (typeof position !== 'number' || Number.isNaN(position)) return 0
    const normalized = position > 1 ? position / SERVER_PROGRESS_SCALE : position
    return Math.max(0, Math.min(1, normalized))
  }

  function currentServerProgressPayload(
    index = currentIndex.value,
    progress = chapterScrollProgress.value
  ) {
    if (!book.value?.bookId) return null
    return {
      bookId: book.value.bookId,
      index,
      position: encodeServerProgress(progress),
    }
  }

  function markProgressDirty() {
    progressDirty.value = true
  }

  /* ─── 本地会话（localStorage：书 + 目录 + 章内进度） ─── */

  function saveReaderSession() {
    if (!book.value || !chapters.value.length) return
    const base = {
      book: book.value,
      currentIndex: currentIndex.value,
      chapterScrollProgress: chapterScrollProgress.value,
      updatedAt: Date.now(),
    }
    const full: PersistedReaderSession = { ...base, chapters: chapters.value }
    // 整本目录可能有几 MB：配额写满时降级为不带目录的瘦身会话，恢复时重新拉目录
    if (!safeLocalSet(READER_SESSION_KEY, JSON.stringify(full))) {
      safeLocalSet(READER_SESSION_KEY, JSON.stringify({ ...base, chapters: [] }))
    }
  }

  // 滚动事件每秒可触发数十次：把整本目录序列化进 localStorage 的会话保存
  // 必须防抖（阅读位置本身另有 120ms 防抖的本地保存与服务端 10s 节流上报，
  // 这里的会话只服务「回到上次阅读页」的恢复，晚 300ms 无感）
  const READER_SESSION_SAVE_DEBOUNCE_MS = 300
  let sessionSaveTimer: ReturnType<typeof setTimeout> | null = null

  function scheduleReaderSessionSave() {
    if (sessionSaveTimer) clearTimeout(sessionSaveTimer)
    sessionSaveTimer = setTimeout(() => {
      sessionSaveTimer = null
      syncLocalBookProgress()
      saveReaderSession()
    }, READER_SESSION_SAVE_DEBOUNCE_MS)
  }

  function flushReaderSessionSave() {
    if (!sessionSaveTimer) return
    clearTimeout(sessionSaveTimer)
    sessionSaveTimer = null
    syncLocalBookProgress()
    saveReaderSession()
  }

  function syncLocalBookProgress(progress = chapterScrollProgress.value) {
    if (!book.value) return
    const encodedProgress = encodeServerProgress(progress)
    book.value.durChapterPos = encodedProgress
    const shelfBook = shelfStore.books.find((item) => item.bookId === book.value?.bookId)
    if (shelfBook) {
      shelfBook.durChapterPos = encodedProgress
    }
  }

  function getPersistedReaderSession(): PersistedReaderSession | null {
    try {
      const raw = localStorage.getItem(READER_SESSION_KEY)
      if (!raw) return null
      return JSON.parse(raw) as PersistedReaderSession
    } catch {
      return null
    }
  }

  /* ─── 服务端上报 ─── */

  async function persistProgress(
    index = currentIndex.value,
    progress = chapterScrollProgress.value
  ) {
    const payload = currentServerProgressPayload(index, progress)
    if (!payload) return
    await saveBookProgress(payload)
      .then(() => {
        progressDirty.value = false
        lastServerProgressKey.value = `${payload.bookId}::${payload.index}::${payload.position}`
      })
      .catch(() => undefined)
  }

  async function flushProgressToServer(force = false) {
    const payload = currentServerProgressPayload()
    if (!payload) return
    const nextKey = `${payload.bookId}::${payload.index}::${payload.position}`
    if (!force && !progressDirty.value && lastServerProgressKey.value === nextKey) return
    await persistProgress(payload.index, chapterScrollProgress.value)
  }

  function flushProgressToServerKeepalive(force = false) {
    const payload = currentServerProgressPayload()
    if (!payload || typeof fetch === 'undefined') return
    const nextKey = `${payload.bookId}::${payload.index}::${payload.position}`
    if (!force && !progressDirty.value && lastServerProgressKey.value === nextKey) return

    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      ...buildAuthHeaders(),
    }

    void fetch(`${API_BASE}/saveBookProgress`, {
      method: 'POST',
      headers,
      body: JSON.stringify(payload),
      keepalive: true,
    }).catch(() => undefined)
    progressDirty.value = false
    lastServerProgressKey.value = nextKey
  }

  return {
    serverProgressTime,
    progressDirty,
    lastServerProgressKey,
    encodeServerProgress,
    decodeServerProgress,
    currentServerProgressPayload,
    markProgressDirty,
    resetProgressTracking,
    saveReaderSession,
    scheduleReaderSessionSave,
    flushReaderSessionSave,
    syncLocalBookProgress,
    getPersistedReaderSession,
    persistProgress,
    flushProgressToServer,
    flushProgressToServerKeepalive,
  }
}
