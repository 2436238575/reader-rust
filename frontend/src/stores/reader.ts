import { defineStore } from 'pinia'
import { API_BASE } from '../utils/appBase'
import { buildAuthHeaders } from '../utils/secureAccess'
import { safeLocalSet } from '../utils/storage'
import { ref, computed, reactive, watch } from 'vue'
import { useAppStore } from './app'
import { useBookshelfStore } from './bookshelf'
import { useAiBookStore } from './aiBook'
import {
  getChapterList,
  getBookContent,
  getChapterImages,
  saveBookProgress,
  setBookSource as apiSetBookSource,
} from '../api/bookshelf'
import {
  getBookmarks,
  saveBookmark,
  deleteBookmark as apiDeleteBookmark,
  deleteBookmarks as apiDeleteBookmarks,
} from '../api/bookmark'
import { getChapterComments, getParaCommentIndex } from '../api/review'
import { getReplaceRules } from '../api/replaceRule'
import type {
  Book,
  BookChapter,
  Bookmark,
  ChapterImage,
  ParaReviewCount,
  ReplaceRule,
  ReviewPage,
} from '../types'
import { getBrowserCachedChapter, setBrowserCachedChapter } from '../utils/browserCache'
import { isLocalBook } from '../utils/localBook'
import { saveRecentReadBook } from '../utils/recentBooks'
import { useReaderTts } from '../composables/useReaderTts'

const READER_SESSION_KEY = 'reader-last-session'
const READER_READ_HISTORY_PREFIX = 'reader-read-history:'
const SERVER_PROGRESS_SCALE = 10000

interface PersistedReaderSession {
  book: Book
  chapters: BookChapter[]
  currentIndex: number
  chapterScrollProgress: number
  updatedAt: number
}

/* ─── Reading config type ─── */
export interface ReadConfig {
  fontSize: number
  fontWeight: number
  fontFamily: string
  lineHeight: number
  paragraphSpacing: number
  firstLineIndent: boolean
  fontColor: string
  pageWidth: number
  pageMode: 'auto' | 'mobile'
  readMethod: '上下滑动' | '左右翻页' | '上下滚动' | '上下滚动2'
  animateDuration: number
  autoPageMode: 'pixel' | 'paragraph'
  scrollPixel: number
  pageSpeed: number
  clickAction: 'next' | 'auto' | 'none'
  selectAction: 'popup' | 'contextmenu' | 'ignore'
  chineseMode: 'simplified' | 'traditional'
  specialMode: 'normal' | 'simple'
  enablePreload: boolean
  /** 显示章节配图（书源配图规则取回的图，以及正文 HTML 里的 <img>） */
  showChapterImages: boolean
}

const defaultConfig: ReadConfig = {
  fontSize: 18,
  fontWeight: 400,
  fontFamily: 'system',
  lineHeight: 1.8,
  paragraphSpacing: 0.2,
  firstLineIndent: true,
  fontColor: '',
  pageWidth: 800,
  pageMode: 'auto',
  readMethod: '上下滑动',
  animateDuration: 300,
  autoPageMode: 'pixel',
  scrollPixel: 1,
  pageSpeed: 1000,
  clickAction: 'auto',
  selectAction: 'ignore',
  chineseMode: 'simplified',
  specialMode: 'normal',
  enablePreload: false,
  showChapterImages: true,
}

function loadConfig(): ReadConfig {
  try {
    const saved = localStorage.getItem('readConfig')
    if (saved) return { ...defaultConfig, ...JSON.parse(saved) }
  } catch {
    /* ignore */
  }
  return { ...defaultConfig }
}

/* ─── Theme presets ─── */
export interface ThemePreset {
  name: string
  body: string
  content: string
  fontColor: string
  popup: string
}

export const themePresets: ThemePreset[] = [
  { name: '默认', body: '#f5ede4', content: '#fff9f0', fontColor: '#333', popup: '#fff' },
  { name: '纯白', body: '#ffffff', content: '#ffffff', fontColor: '#333', popup: '#fff' },
  { name: '琥珀', body: '#f5e6ce', content: '#faf0e4', fontColor: '#5b4636', popup: '#faf0e4' },
  { name: '薄荷', body: '#e0f0e8', content: '#eaf5ef', fontColor: '#2d4a3e', popup: '#eaf5ef' },
  { name: '天蓝', body: '#dce8f0', content: '#e8f0f6', fontColor: '#2c3e50', popup: '#e8f0f6' },
  { name: '粉白', body: '#f5e4e8', content: '#faf0f3', fontColor: '#4a2d36', popup: '#faf0f3' },
  { name: '浅灰', body: '#eaeaea', content: '#f5f5f5', fontColor: '#333', popup: '#f5f5f5' },
  { name: '暗灰', body: '#808080', content: '#999', fontColor: '#eee', popup: '#888' },
  { name: '暗夜', body: '#141414', content: '#16213e', fontColor: '#c8c8c8', popup: '#141414' },
]

/* ─── Font presets ─── */
export const fontPresets = [
  { label: '系统', value: 'system', family: '' },
  { label: '黑体', value: 'heiti', family: '"SimHei", "STHeiti", "Heiti SC", sans-serif' },
  { label: '楷体', value: 'kaiti', family: '"KaiTi", "STKaiti", "BiauKai", serif' },
  { label: '宋体', value: 'songti', family: '"SimSun", "STSong", "Songti SC", serif' },
  { label: '仿宋', value: 'fangsong', family: '"FangSong", "STFangsong", serif' },
]

export const useReaderStore = defineStore('reader', () => {
  type ReaderPanel =
    'catalog' | 'settings' | 'bookshelf' | 'source' | 'bookmark' | 'rule' | 'cache' | null
  const appStore = useAppStore()
  const shelfStore = useBookshelfStore()
  const aiBookStore = useAiBookStore()
  const book = ref<Book | null>(null)
  const chapters = ref<BookChapter[]>([])
  const currentIndex = ref(0)
  const content = ref('')
  const loading = ref(false)
  const chaptersLoading = ref(false)
  /// 打开书籍失败的原因。有值时阅读页显示错误而不是「加载中...」占位符
  const loadError = ref('')
  const bookmarks = ref<Bookmark[]>([])
  const replaceRules = ref<ReplaceRule[]>([])
  /* 评论（章评 / 段评）：由书源规则决定是否可用 */
  const reviewEnabled = ref(false)
  const paraReviewEnabled = ref(false)
  const reviewAuthorMarks = ref(false)
  const paraReviewIndex = ref<ParaReviewCount[]>([])
  const chapterCommentTotal = ref(0)
  const chapterComments = ref<ReviewPage | null>(null)
  const reviewsLoading = ref(false)
  /* 章节配图：书源声明了配图规则时才有 */
  const chapterImages = ref<ChapterImage[]>([])
  const chapterImagesEnabled = ref(false)
  const preloadedContent = ref<Map<number, string>>(new Map()) // index -> content
  const isAutoScrolling = ref(false)
  const chapterScrollProgress = ref(0)
  const readChapterKeys = ref<Set<string>>(new Set())
  const progressDirty = ref(false)
  const lastServerProgressKey = ref('')

  const currentChapter = computed(() => chapters.value[currentIndex.value] || null)
  const hasNext = computed(() => currentIndex.value < chapters.value.length - 1)
  const hasPrev = computed(() => currentIndex.value > 0)

  const readingProgress = computed(() => {
    if (chapters.value.length === 0) return '0%'
    const progress =
      ((currentIndex.value + chapterScrollProgress.value) / chapters.value.length) * 100
    const normalized = Math.max(0, Math.min(100, progress))
    return `${normalized < 10 ? normalized.toFixed(1) : Math.round(normalized)}%`
  })

  /* ─── Reading config ─── */
  const config = reactive<ReadConfig>(loadConfig())

  function saveConfig() {
    safeLocalSet('readConfig', JSON.stringify(config))
  }

  function updateConfig<K extends keyof ReadConfig>(key: K, value: ReadConfig[K]) {
    config[key] = value
    if (key === 'enablePreload' && !value) {
      preloadedContent.value.clear()
    }
    saveConfig()
  }

  function resetConfig() {
    Object.assign(config, defaultConfig)
    saveConfig()
  }

  // 两个方向各自按需加载（cn2t=简→繁、t2cn=繁→简，字典互不重叠），
  // 不用 full 包就是为了避免把没在用的那份字典也拉下来
  const s2tConverter = ref<((text: string) => string) | null>(null)
  const t2sConverter = ref<((text: string) => string) | null>(null)
  let s2tLoading: Promise<void> | null = null
  let t2sLoading: Promise<void> | null = null

  function ensureChineseConverterLoaded() {
    if (config.chineseMode === 'traditional') {
      if (s2tConverter.value || s2tLoading) return s2tLoading ?? Promise.resolve()
      // 只引 cn2t 子路径（而非 full 包）：词级字典只带简→繁一份，按需动态加载
      s2tLoading = import('opencc-js/cn2t')
        .then((module) => {
          s2tConverter.value = module.Converter({ from: 'cn', to: 'tw' })
        })
        .catch(() => {
          s2tConverter.value = null
        })
        .finally(() => {
          s2tLoading = null
        })
      return s2tLoading
    }
    if (t2sConverter.value || t2sLoading) return t2sLoading ?? Promise.resolve()
    t2sLoading = import('opencc-js/t2cn')
      .then((module) => {
        t2sConverter.value = module.Converter({ from: 'tw', to: 'cn' })
      })
      .catch(() => {
        t2sConverter.value = null
      })
      .finally(() => {
        t2sLoading = null
      })
    return t2sLoading
  }

  /* ─── Theme ─── */
  const themeIndex = ref(parseInt(localStorage.getItem('reader-themeIndex') || '0'))
  const isNight = computed({
    get: () => appStore.theme === 'dark',
    set: (value: boolean) => {
      appStore.setTheme(value ? 'dark' : 'light')
      safeLocalSet('reader-isNight', String(value))
    },
  })

  const currentTheme = computed(() => {
    if (isNight.value) return themePresets[themePresets.length - 1]
    return themePresets[themeIndex.value] || themePresets[0]
  })

  function setThemeIndex(idx: number) {
    themeIndex.value = idx
    isNight.value = false
    safeLocalSet('reader-themeIndex', String(idx))
    safeLocalSet('reader-isNight', 'false')
  }

  function toggleNight() {
    isNight.value = !isNight.value
  }

  /* ─── Chinese Conversion (OpenCC) ─── */
  /* ─── Content Filtering (Replace Rules) ─── */
  function applyReplaceRules(text: string) {
    if (!text) return ''
    let result = text
    const currentBook = book.value

    function matchRuleScope(rule: ReplaceRule) {
      const scope = (rule.scope || '').trim()
      if (!scope || scope === '*') return true
      if (!currentBook) return false

      if (scope.startsWith('source:')) {
        return scope.slice('source:'.length) === currentBook.origin
      }

      if (scope.startsWith('book:')) {
        return scope.slice('book:'.length) === currentBook.bookUrl
      }

      const scopeParts = scope.split(';')
      if (scopeParts[0] !== '*' && scopeParts[0] !== currentBook.name) {
        return false
      }
      return scopeParts.length === 1 || scopeParts[1] === currentBook.bookUrl
    }

    // Sort by order and apply enabled rules
    const enabledRules = [...replaceRules.value]
      .filter((r) => r.isEnabled && matchRuleScope(r))
      .sort((a, b) => a.order - b.order)

    for (const rule of enabledRules) {
      try {
        if (rule.isRegex) {
          const re = new RegExp(rule.pattern, 'gm')
          result = result.replace(re, rule.replacement)
        } else {
          result = result.replaceAll(rule.pattern, rule.replacement)
        }
      } catch (e) {
        console.error(`Failed to apply rule: ${rule.name}`, e)
      }
    }
    return result
  }

  function convertContent(text: string) {
    if (!text) return text
    // 按当前模式取对应方向的转换器；切换模式后转换器若还在加载，
    // 先显示原文——converter 是 ref，加载完成时 displayContent 会自动重算
    const converter = config.chineseMode === 'traditional' ? s2tConverter.value : t2sConverter.value
    return converter ? converter(text) : text
  }

  function processContentForDisplay(text: string) {
    return convertContent(applyReplaceRules(text))
  }

  const displayContent = computed(() => {
    return processContentForDisplay(content.value)
  })

  watch(
    () => config.chineseMode,
    (mode) => {
      if (mode === 'traditional') {
        void ensureChineseConverterLoaded()
      }
    },
    { immediate: true }
  )

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
    if (!book.value) return null
    return {
      bookUrl: book.value.bookUrl,
      index,
      position: encodeServerProgress(progress),
    }
  }

  function markProgressDirty() {
    progressDirty.value = true
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
    const shelfBook = shelfStore.books.find((item) => item.bookUrl === book.value?.bookUrl)
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

  async function restorePersistedSession() {
    const session = getPersistedReaderSession()
    if (!session?.book) return false

    book.value = session.book
    chapters.value = session.chapters ?? []
    // 瘦身会话（配额降级时落的盘）不带目录：恢复时重新拉一次
    if (!chapters.value.length) {
      try {
        chapters.value = await getChapterList({
          bookUrl: session.book.bookUrl,
          bookSourceUrl: session.book.origin,
        })
      } catch {
        return false
      }
    }
    if (!chapters.value.length) return false
    loadReadChapterHistory(session.book)

    const nextIndex = Math.max(0, Math.min(session.currentIndex || 0, session.chapters.length - 1))
    try {
      const chapterContent = await fetchChapterContent(nextIndex)
      if (chapterContent == null) return false
      setActiveChapterState(nextIndex, chapterContent, session.chapterScrollProgress || 0)
      markChapterAsRead(nextIndex)
      // 恢复会话不经过 loadChapter，评论要在这里单独补一次
      void loadChapterReviews(nextIndex)
      return true
    } catch {
      return false
    }
  }

  function getReadHistoryStorageKey(currentBook?: Book | null) {
    if (!currentBook?.bookUrl) return ''
    return `${READER_READ_HISTORY_PREFIX}${currentBook.bookUrl}`
  }

  function buildReadChapterKey(
    index: number,
    chapter?: BookChapter | null,
    currentBook?: Book | null
  ) {
    if (!currentBook?.bookUrl) return ''
    const sourceKey = currentBook.origin || 'default'
    if (chapter?.url) {
      return `${currentBook.bookUrl}::${sourceKey}::${chapter.url}`
    }
    return `${currentBook.bookUrl}::${sourceKey}::index:${index}`
  }

  function loadReadChapterHistory(currentBook?: Book | null) {
    const storageKey = getReadHistoryStorageKey(currentBook)
    if (!storageKey) {
      readChapterKeys.value = new Set()
      return
    }
    try {
      const raw = localStorage.getItem(storageKey)
      if (!raw) {
        readChapterKeys.value = new Set()
        return
      }
      const parsed = JSON.parse(raw)
      readChapterKeys.value = new Set(
        Array.isArray(parsed) ? parsed.filter((item) => typeof item === 'string') : []
      )
    } catch {
      readChapterKeys.value = new Set()
    }
  }

  function persistReadChapterHistory(currentBook?: Book | null) {
    const storageKey = getReadHistoryStorageKey(currentBook)
    if (!storageKey) return
    safeLocalSet(storageKey, JSON.stringify(Array.from(readChapterKeys.value)))
  }

  function markChapterAsRead(index: number) {
    const key = buildReadChapterKey(index, chapters.value[index], book.value)
    if (!key || readChapterKeys.value.has(key)) return
    const next = new Set(readChapterKeys.value)
    next.add(key)
    readChapterKeys.value = next
    persistReadChapterHistory(book.value)
  }

  function isChapterRead(index: number) {
    return readChapterKeys.value.has(buildReadChapterKey(index, chapters.value[index], book.value))
  }

  /* ─── Auto reading ─── */
  // isAutoScrolling 是唯一状态；ReaderView 直接 watch 它驱动滚动

  function toggleAutoReading() {
    isAutoScrolling.value = !isAutoScrolling.value
  }

  function stopAutoReading() {
    isAutoScrolling.value = false
  }

  /* ─── TTS：状态机在 composables/useReaderTts（唯一外部依赖是当前章正文） ─── */
  const tts = useReaderTts(content)

  /* ─── Book / chapter ops ─── */
  let bookLoadSeq = 0
  // 打开这本书时服务端进度的真实保存时间。setActiveChapterState 会把
  // book.durChapterTime 刷成 Date.now()（书架排序需要），本地/服务端进度
  // 比新旧时若直接用被刷新的值，服务端会靠这个伪造的新鲜时间戳「永远获胜」，
  // 离线读了几章没同步成功的本地进度就被误判为旧数据丢弃
  const serverProgressTime = ref(0)

  async function loadBook(b: Book) {
    // 连点两本书时目录响应可能乱序到达：只认最后一次打开的书。
    // 同时作废在途的 loadChapter，免得旧书正文写到新书上。
    const loadSeq = ++bookLoadSeq
    chapterLoadSeq++
    loading.value = true
    loadError.value = ''
    book.value = b
    serverProgressTime.value = b.durChapterTime || 0
    chapters.value = []
    content.value = ''
    resetReviews()
    appStore.markBookOpened(b.bookUrl)
    currentIndex.value = b.durChapterIndex || 0
    chapterScrollProgress.value = 0
    preloadedContent.value.clear()
    loadReadChapterHistory(b)
    progressDirty.value = false
    lastServerProgressKey.value = ''
    chaptersLoading.value = true
    try {
      const list = await getChapterList({
        bookUrl: b.bookUrl,
        bookSourceUrl: b.origin,
      })
      if (loadSeq !== bookLoadSeq) return
      chapters.value = list
      saveReaderSession()
    } catch (error) {
      // 目录都拿不到就等于这本书打不开。清掉半开状态并记下原因：否则阅读页
      // 会一直停在「加载中...」的占位符上，用户完全看不出发生了什么。
      if (loadSeq === bookLoadSeq) {
        loading.value = false
        chapters.value = []
        content.value = ''
        loadError.value = (error as Error)?.message || '打开书籍失败'
        appStore.showToast(loadError.value, 'error')
      }
      throw error
    } finally {
      if (loadSeq === bookLoadSeq) chaptersLoading.value = false
    }
  }

  function setActiveChapterState(index: number, chapterContent: string, progress = 0) {
    currentIndex.value = index
    content.value = chapterContent
    // 配图跟着章节走：先清掉上一章的，等本次请求回来再填
    resetChapterImages()
    chapterScrollProgress.value = Math.max(0, Math.min(1, progress))
    if (book.value) {
      book.value.durChapterIndex = index
      book.value.durChapterTitle = chapters.value[index]?.title || book.value.durChapterTitle
      book.value.durChapterTime = Date.now()
      const shelfBook = shelfStore.books.find((item) => item.bookUrl === book.value?.bookUrl)
      if (shelfBook) {
        shelfBook.durChapterIndex = book.value.durChapterIndex
        shelfBook.durChapterTitle = book.value.durChapterTitle
        shelfBook.durChapterTime = book.value.durChapterTime
      }
    }
    syncLocalBookProgress(chapterScrollProgress.value)
    if (book.value) {
      saveRecentReadBook(book.value)
    }
    safeLocalSet('reader-currentIndex', String(index))
    saveReaderSession()
    markProgressDirty()
  }

  async function persistProgress(
    index = currentIndex.value,
    progress = chapterScrollProgress.value
  ) {
    const payload = currentServerProgressPayload(index, progress)
    if (!payload) return
    await saveBookProgress(payload)
      .then(() => {
        progressDirty.value = false
        lastServerProgressKey.value = `${payload.bookUrl}::${payload.index}::${payload.position}`
      })
      .catch(() => undefined)
  }

  async function flushProgressToServer(force = false) {
    const payload = currentServerProgressPayload()
    if (!payload) return
    const nextKey = `${payload.bookUrl}::${payload.index}::${payload.position}`
    if (!force && !progressDirty.value && lastServerProgressKey.value === nextKey) return
    await persistProgress(payload.index, chapterScrollProgress.value)
  }

  function flushProgressToServerKeepalive(force = false) {
    const payload = currentServerProgressPayload()
    if (!payload || typeof fetch === 'undefined') return
    const nextKey = `${payload.bookUrl}::${payload.index}::${payload.position}`
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

  async function fetchChapterContent(index: number, forceRefresh = false) {
    if (!book.value || !chapters.value[index]) return null

    if (!forceRefresh && preloadedContent.value.has(index)) {
      return preloadedContent.value.get(index) || null
    }

    const chapter = chapters.value[index]

    const isLocal = isLocalBook(book.value)
    const useBrowserCache = !isLocal
    const browserCached = useBrowserCache
      ? await getBrowserCachedChapter(book.value.bookUrl, chapter.url).catch(() => null)
      : null

    if (!forceRefresh && browserCached) {
      return browserCached
    }

    if (!appStore.isOnline && !isLocal) {
      if (browserCached) {
        return browserCached
      }
      throw new Error('当前处于离线状态，且该章节未缓存到浏览器')
    }

    let chapterContent = ''
    try {
      chapterContent = await getBookContent({
        chapterUrl: chapter.url,
        bookUrl: book.value.bookUrl,
        bookSourceUrl: book.value.origin,
        refresh: forceRefresh ? 1 : 0,
      })
    } catch (error) {
      if (browserCached) {
        appStore.showToast('网络请求失败，已切换到本地缓存章节', 'warning')
        return browserCached
      }
      throw error
    }

    if (useBrowserCache) {
      await setBrowserCachedChapter({
        bookUrl: book.value.bookUrl,
        chapterUrl: chapter.url,
        chapterTitle: chapter.title,
        content: chapterContent,
      }).catch(() => undefined)
    }

    return chapterContent
  }

  let chapterLoadSeq = 0
  // 加载失败的目标章：重试时要重拉的是它，而不是还停留在旧章的 currentIndex
  const failedChapterIndex = ref<number | null>(null)

  async function loadChapter(index: number, forceRefresh = false) {
    if (!book.value || !chapters.value[index]) return

    // 快速连点下一章时旧响应可能后到：只认最新一次请求的结果
    const loadSeq = ++chapterLoadSeq
    loading.value = true
    loadError.value = ''
    failedChapterIndex.value = null
    try {
      const chapterContent = await fetchChapterContent(index, forceRefresh)
      if (chapterContent == null) return
      if (loadSeq !== chapterLoadSeq) return

      const previousSavedIndex = book.value.durChapterIndex ?? 0
      const previousSavedProgress = decodeServerProgress(book.value.durChapterPos)
      const isOpeningSavedChapter = !forceRefresh && index === previousSavedIndex
      const initialProgress = isOpeningSavedChapter ? previousSavedProgress : 0

      setActiveChapterState(index, chapterContent, initialProgress)
      markChapterAsRead(index)
      appStore.markChapterRead(book.value.bookUrl, index, chapters.value.length)

      if (!isOpeningSavedChapter) {
        await persistProgress(index, 0)
      }

      // 正文先渲染，评论与配图随后补上：拉不到不该拖慢或打断阅读
      void loadChapterReviews(index)
      void loadChapterImages(index)

      if (config.enablePreload) {
        setTimeout(() => preloadAroundChapter(index), forceRefresh ? 1500 : 1000)
      }
    } catch (error) {
      // 失败要留下可见的错误态（正文区展示 + 重试入口），只清 loading 会让用户
      // 对着旧章或空白分不清「还在加载」还是「已经失败」。不再向外抛：所有
      // 调用方（翻章/目录/开书）都不接异常，抛出去只会变成 unhandled rejection。
      if (loadSeq === chapterLoadSeq) {
        loadError.value = (error as Error)?.message || '章节加载失败'
        failedChapterIndex.value = index
        appStore.showToast(loadError.value, 'error')
      }
    } finally {
      // 过期请求的 finally 不能灭掉新一轮加载的 loading
      if (loadSeq === chapterLoadSeq) loading.value = false
    }
  }

  /** 失败重试：目录都没拿到就整书重载，否则重拉失败时目标章（无记录退回当前章） */
  async function retryLoad() {
    if (!book.value) return
    try {
      if (!chapters.value.length) {
        await loadBook(book.value)
        await loadChapter(currentIndex.value)
      } else {
        await loadChapter(failedChapterIndex.value ?? currentIndex.value)
      }
    } catch {
      // loadBook 失败时已自行 toast，这里不再重复提示
    }
  }

  /* ─── 评论（章评 / 段评） ─── */

  /**
   * 拉取当前章节的段评概览与章评第一页。
   *
   * 书源没声明评论规则时后端返回 `enabled: false`，此时前端不渲染任何入口；
   * 网络或解析失败同样静默降级——评论是附加内容，不能让它影响正文阅读。
   */
  async function loadChapterReviews(index = currentIndex.value) {
    const currentBook = book.value
    const chapter = chapters.value[index]
    if (!currentBook || !chapter || isLocalBook(currentBook)) {
      resetReviews()
      return
    }
    // 只认自己那次请求的结果，避免快速翻章时旧响应覆盖新章节
    const requestedChapterUrl = chapter.url
    reviewsLoading.value = true
    try {
      const params = {
        bookUrl: currentBook.bookUrl,
        chapterUrl: requestedChapterUrl,
        bookSourceUrl: currentBook.origin,
      }
      // 顺序请求：第二个请求能复用第一个已经取回的章节正文响应
      const indexResp = await getParaCommentIndex(params)
      const chapterResp = await getChapterComments({ ...params, page: 1 })
      if (chapters.value[currentIndex.value]?.url !== requestedChapterUrl) return
      paraReviewEnabled.value = indexResp.enabled
      paraReviewIndex.value = indexResp.data?.paras || []
      reviewEnabled.value = chapterResp.enabled
      reviewAuthorMarks.value = chapterResp.authorMarks === true
      chapterCommentTotal.value = chapterResp.data?.total || 0
      chapterComments.value = chapterResp.data || null
    } catch {
      if (chapters.value[currentIndex.value]?.url === requestedChapterUrl) resetReviews()
    } finally {
      reviewsLoading.value = false
    }
  }

  function resetReviews() {
    reviewEnabled.value = false
    paraReviewEnabled.value = false
    reviewAuthorMarks.value = false
    paraReviewIndex.value = []
    chapterCommentTotal.value = 0
    chapterComments.value = null
  }

  /**
   * 拉取本章配图。
   *
   * 与评论同理：书源没声明配图规则时后端返回 `enabled: false`，前端不渲染配图区；
   * 失败静默降级——配图是附加内容，不能影响正文阅读。
   * 后端不缓存配图（图片地址带时效签名），所以这里也不做本地缓存。
   */
  async function loadChapterImages(index = currentIndex.value) {
    const currentBook = book.value
    const chapter = chapters.value[index]
    if (!currentBook || !chapter || isLocalBook(currentBook)) {
      resetChapterImages()
      return
    }
    // 只认自己那次请求的结果，避免快速翻章时旧响应覆盖新章节
    const requestedChapterUrl = chapter.url
    try {
      const resp = await getChapterImages({
        bookUrl: currentBook.bookUrl,
        chapterUrl: requestedChapterUrl,
        bookSourceUrl: currentBook.origin,
      })
      if (chapters.value[currentIndex.value]?.url !== requestedChapterUrl) return
      chapterImagesEnabled.value = resp?.enabled === true
      chapterImages.value = resp?.images || []
    } catch {
      if (chapters.value[currentIndex.value]?.url === requestedChapterUrl) resetChapterImages()
    }
  }

  function resetChapterImages() {
    chapterImagesEnabled.value = false
    chapterImages.value = []
  }

  /** 段号 → 该段评论条数，供正文渲染气泡时查表。 */
  const paraReviewCountByIndex = computed(() => {
    const map = new Map<number, ParaReviewCount>()
    if (!paraReviewEnabled.value) return map
    for (const item of paraReviewIndex.value) map.set(item.paraIndex, item)
    return map
  })

  async function preloadAroundChapter(index: number) {
    if (!book.value || !config.enablePreload) return
    const targets = [index + 1, index + 2, index - 1].filter(
      (target, pos, list) =>
        target >= 0 && target < chapters.value.length && list.indexOf(target) === pos
    )
    for (const target of targets) {
      await preloadNextChapter(target)
    }
  }

  async function preloadNextChapter(index: number) {
    if (
      !book.value ||
      !config.enablePreload ||
      index >= chapters.value.length ||
      preloadedContent.value.has(index)
    )
      return

    // Keep max 3 preloaded chapters
    if (preloadedContent.value.size > 3) {
      const firstKey = preloadedContent.value.keys().next().value
      if (firstKey !== undefined) preloadedContent.value.delete(firstKey)
    }

    try {
      const res = await fetchChapterContent(index)
      if (!res) return
      preloadedContent.value.set(index, res)
    } catch {
      /* ignore */
    }
  }

  function normalizeChapterTitle(title?: string) {
    return (title || '')
      .replace(/\s+/g, '')
      .replace(/[^\p{L}\p{N}]/gu, '')
      .toLowerCase()
  }

  function resolveChapterIndexByTitle(
    list: BookChapter[],
    targetTitle?: string,
    fallbackIndex = 0
  ) {
    if (!list.length) return 0
    const normalizedTarget = normalizeChapterTitle(targetTitle)
    if (!normalizedTarget) {
      return Math.max(0, Math.min(list.length - 1, fallbackIndex))
    }

    const exactIndex = list.findIndex(
      (chapter) => normalizeChapterTitle(chapter.title) === normalizedTarget
    )
    if (exactIndex >= 0) return exactIndex

    const partialIndex = list.findIndex((chapter) => {
      const title = normalizeChapterTitle(chapter.title)
      return title.includes(normalizedTarget) || normalizedTarget.includes(title)
    })
    if (partialIndex >= 0) return partialIndex

    return Math.max(0, Math.min(list.length - 1, fallbackIndex))
  }

  /* ─── Switch Source ─── */
  async function switchSource(newUrl: string, sourceUrl: string) {
    if (!book.value) return
    const previousChapterTitle = currentChapter.value?.title || book.value.durChapterTitle
    const previousIndex = currentIndex.value
    const previousProgress = chapterScrollProgress.value
    loading.value = true
    try {
      const updatedBook = await apiSetBookSource({
        bookUrl: book.value.bookUrl,
        newUrl,
        bookSourceUrl: sourceUrl,
      })
      if (!updatedBook) return null

      await loadBook(updatedBook)
      const targetIndex = resolveChapterIndexByTitle(
        chapters.value,
        previousChapterTitle,
        typeof updatedBook.durChapterIndex === 'number'
          ? updatedBook.durChapterIndex
          : previousIndex
      )
      await loadChapter(targetIndex)
      setChapterScrollProgress(previousProgress)
      await shelfStore.fetchBooks().catch(() => undefined)
      return updatedBook
    } finally {
      loading.value = false
    }
  }

  async function refreshContent() {
    if (!book.value || !chapters.value[currentIndex.value]) return
    // 快照当前章节：await 期间用户可能已翻章，旧正文不能盖到新章上。
    // 与 loadChapter 共用序号，翻章后迟到的刷新结果直接作废。
    const index = currentIndex.value
    const loadSeq = ++chapterLoadSeq
    loading.value = true
    try {
      const chapterContent = await fetchChapterContent(index, true)
      if (chapterContent == null) return
      if (loadSeq !== chapterLoadSeq || index !== currentIndex.value) return
      setActiveChapterState(index, chapterContent, chapterScrollProgress.value)
      void preloadAroundChapter(index)
    } catch (error) {
      // 手动刷新失败不能静默：转圈停了用户会以为刷新成功
      if (loadSeq === chapterLoadSeq) {
        appStore.showToast((error as Error)?.message || '刷新章节失败', 'error')
      }
    } finally {
      if (loadSeq === chapterLoadSeq) loading.value = false
    }
  }

  async function refreshChapters() {
    if (!book.value) return
    chaptersLoading.value = true
    try {
      preloadedContent.value.clear()
      // 用户要的是刷新目录列表，不是重读当前章：此前 loadChapter(target, true)
      // 会把人踢回章首。现在只换列表，当前章正文与阅读位置不动
      const currentUrl = chapters.value[currentIndex.value]?.url
      chapters.value = await getChapterList({
        bookUrl: book.value.bookUrl,
        bookSourceUrl: book.value.origin,
        refresh: 1,
      })
      // 上游增删章导致位置漂移时，按 URL 把索引对齐回去
      if (currentUrl) {
        const newIndex = chapters.value.findIndex((chapter) => chapter.url === currentUrl)
        if (newIndex >= 0 && newIndex !== currentIndex.value) {
          currentIndex.value = newIndex
          book.value.durChapterIndex = newIndex
        }
      }
    } finally {
      chaptersLoading.value = false
    }
  }

  function setChapterScrollProgress(value: number) {
    chapterScrollProgress.value = Math.max(0, Math.min(1, value))
    markProgressDirty()
    scheduleReaderSessionSave()
  }

  async function nextChapter() {
    if (hasNext.value) {
      const completedBook = book.value ? { ...book.value } : null
      const completedChapter = currentChapter.value ? { ...currentChapter.value } : null
      const completedContent = content.value
      await loadChapter(currentIndex.value + 1)
      if (completedBook && completedChapter && completedContent) {
        void aiBookStore.autoUpdateCompletedChapter({
          book: completedBook,
          chapter: completedChapter,
          chapterContent: completedContent,
          chapters: chapters.value,
        })
      }
    }
  }

  async function prevChapter() {
    if (hasPrev.value) {
      await loadChapter(currentIndex.value - 1)
    }
  }

  /* ─── Replace Rules ─── */
  async function fetchReplaceRules() {
    try {
      replaceRules.value = await getReplaceRules()
    } catch {
      /* ignore */
    }
  }

  /* ─── Bookmarks ─── */
  async function fetchBookmarks() {
    try {
      const all = await getBookmarks()
      // Filter for current book
      if (book.value) {
        bookmarks.value = all.filter(
          (b) => b.bookName === book.value?.name && b.bookAuthor === book.value?.author
        )
      } else {
        bookmarks.value = all
      }
    } catch {
      /* ignore */
    }
  }

  async function addBookmark(pos: number = 0, snippet: string = '') {
    if (!book.value || !currentChapter.value) return
    const b: Bookmark = {
      bookName: book.value.name,
      bookAuthor: book.value.author,
      chapterIndex: currentIndex.value,
      chapterName: currentChapter.value.title,
      chapterPos: pos,
      bookText: snippet || content.value.slice(0, 50).replace(/<[^>]+>/g, ''),
      time: Date.now(),
      content: '',
    }
    await saveBookmark(b)
    await fetchBookmarks()
  }

  async function removeBookmark(b: Bookmark) {
    await apiDeleteBookmark(b)
    await fetchBookmarks()
  }

  async function removeBookmarks(items: Bookmark[]) {
    if (!items.length) return
    await apiDeleteBookmarks(items)
    await fetchBookmarks()
  }

  function clear() {
    book.value = null
    chapters.value = []
    content.value = ''
    currentIndex.value = 0
    chapterScrollProgress.value = 0
    readChapterKeys.value = new Set()
    serverProgressTime.value = 0
    resetReviews()
    stopAutoReading()
  }

  /* ─── Panel visibility ─── */
  const activePanel = ref<ReaderPanel>(null)
  const panelParent = ref<ReaderPanel>(null)

  function openPanel(panel: ReaderPanel, parent: ReaderPanel = null) {
    activePanel.value = panel
    panelParent.value = parent
  }

  function togglePanel(panel: ReaderPanel, parent: ReaderPanel = null) {
    if (activePanel.value === panel) {
      closePanel()
      return
    }
    openPanel(panel, parent)
  }

  function backPanel() {
    if (panelParent.value) {
      activePanel.value = panelParent.value
      panelParent.value = null
      return
    }
    activePanel.value = null
  }

  function closePanel() {
    activePanel.value = null
    panelParent.value = null
  }

  return {
    book,
    chapters,
    currentIndex,
    content,
    loading,
    chaptersLoading,
    loadError,
    serverProgressTime,
    currentChapter,
    hasNext,
    hasPrev,
    readingProgress,
    loadBook,
    loadChapter,
    retryLoad,
    fetchChapterContent,
    setActiveChapterState,
    refreshContent,
    nextChapter,
    prevChapter,
    clear,
    chapterScrollProgress,
    setChapterScrollProgress,
    flushReaderSessionSave,
    getPersistedReaderSession,
    restorePersistedSession,
    persistProgress,
    flushProgressToServer,
    flushProgressToServerKeepalive,
    config,
    updateConfig,
    resetConfig,
    saveConfig,
    themeIndex,
    isNight,
    currentTheme,
    setThemeIndex,
    toggleNight,
    toggleAutoReading,
    stopAutoReading,
    activePanel,
    openPanel,
    togglePanel,
    backPanel,
    closePanel,
    bookmarks,
    fetchBookmarks,
    addBookmark,
    removeBookmark,
    removeBookmarks,
    readChapterKeys,
    isChapterRead,
    markChapterAsRead,
    replaceRules,
    fetchReplaceRules,
    switchSource,
    preloadNextChapter,
    preloadAroundChapter,
    refreshChapters,
    ...tts,
    displayContent,
    processContentForDisplay,
    isAutoScrolling,
    reviewEnabled,
    paraReviewEnabled,
    reviewAuthorMarks,
    paraReviewIndex,
    paraReviewCountByIndex,
    chapterCommentTotal,
    chapterComments,
    reviewsLoading,
    loadChapterReviews,
    resetReviews,
    chapterImages,
    chapterImagesEnabled,
    loadChapterImages,
    resetChapterImages,
  }
})
