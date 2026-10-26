import { defineStore } from 'pinia'
import { safeLocalSet } from '../utils/storage'
import { ref, computed, reactive, watch } from 'vue'
import { useAppStore } from './app'
import { useBookshelfStore } from './bookshelf'
import { useAiBookStore } from './aiBook'
import {
  getChapterList,
  getBookContent,
  getBookContents,
  getChapterImages,
  setBookSource as apiSetBookSource,
} from '../api/bookshelf'
import {
  getBookmarks,
  saveBookmark,
  deleteBookmark as apiDeleteBookmark,
  deleteBookmarks as apiDeleteBookmarks,
} from '../api/bookmark'
import { getChapterComments, getParaCommentIndex } from '../api/review'
import { getUserdata } from '../api/userdata'
import { queueUserdataSave } from '../utils/userdataQueue'
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
import {
  deleteBrowserCachedChapter,
  deleteBrowserCachedChapters,
  getBrowserCachedChapter,
  getBrowserCatalog,
  setBrowserCachedChapter,
  setBrowserCatalog,
} from '../utils/browserCache'
import { isLocalBook } from '../utils/localBook'
import { describeRequestFailure } from '../utils/requestFailure'
import { saveRecentReadBook } from '../utils/recentBooks'
import { listChapterEdits, selectChapterVersion } from '../api/chapterEdit'
import type {
  ChapterContentResponse,
  ChapterVersionKind,
  ContentVariant,
} from '../api/bookshelf'
import { hydrateDiffOps, type ParagraphDiffOp } from '../utils/chapterDiff'
import {
  loadLocalKeywordRules,
  persistAndPushKeywordRules,
  syncKeywordRulesFromBackend,
  type KeywordHighlightRule,
} from '../utils/keywordHighlights'
import { useReaderTts } from '../composables/useReaderTts'
import { createReaderProgress } from './readerProgress'

const READER_READ_HISTORY_PREFIX = 'reader-read-history:'

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
  readMethod: '上下滑动' | '左右翻页' | '双页翻页' | '上下滚动' | '上下滚动2'
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
  /** 已编辑章节的正文 diff 标记（绿/蓝竖线、红箭头、段内虚线） */
  showDiffMarks: boolean
  /** 正文版本：编辑版优先，或只读上游原版（只对编辑过的章节有区别） */
  contentVersion: 'edited' | 'upstream'
  /** 评论里回复的配图形态：缩略图直接显示，或番茄式「查看图片」文字链 */
  replyImageStyle: 'thumbnail' | 'link'
}

/** 某一章的编辑版差异状态（由正文接口带回）。 */
export interface ChapterDiffState {
  edited: boolean
  ops: ParagraphDiffOp[] | null
  /** diff 算不出来的原因；空串表示正常 */
  error: string
  /** 当前显示的版本：上游原版不叠加 diff 标记（但面板仍可对照） */
  variant: ContentVariant
}

/** 某一章当前版本的定位信息（由正文接口带回）。 */
export interface ChapterVersionState {
  versionId?: number
  kind?: ChapterVersionKind
  baseVersionId?: number
  latestUserVersionId?: number
  latestUpstreamVersionId?: number
  upstreamChanged: boolean
  selectionSource: 'selected' | 'edited' | 'upstream' | 'upstream_preview'
  stale: boolean
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
  enablePreload: true,
  showChapterImages: true,
  showDiffMarks: true,
  contentVersion: 'edited',
  replyImageStyle: 'thumbnail',
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

/* 已编辑章节集合的本地快照：离线打开书时 listChapterEdits 拉不到，
   没有快照就会把编辑章当成普通章节，从浏览器缓存读到编辑前的旧正文。
   只保留最近 20 本书；写满或数据损坏时静默放弃，在线打开会重新同步。 */
const EDITED_CHAPTERS_CACHE_KEY = 'reader-edited-chapters'
const EDITED_CHAPTERS_CACHE_MAX_BOOKS = 20

interface EditedChaptersCacheEntry {
  urls: string[]
  savedAt: number
}

function readEditedChaptersCache(): Record<string, EditedChaptersCacheEntry> {
  try {
    const raw = localStorage.getItem(EDITED_CHAPTERS_CACHE_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw) as Record<string, EditedChaptersCacheEntry>
    return parsed && typeof parsed === 'object' ? parsed : {}
  } catch {
    return {}
  }
}

function loadCachedEditedChapterKeys(bookId: string): string[] {
  const entry = readEditedChaptersCache()[bookId]
  return Array.isArray(entry?.urls)
    ? entry.urls.filter((url): url is string => typeof url === 'string')
    : []
}

function saveCachedEditedChapterKeys(bookId: string, urls: string[]) {
  try {
    const cache = readEditedChaptersCache()
    cache[bookId] = { urls, savedAt: Date.now() }
    const trimmed = Object.entries(cache)
      .sort((a, b) => (b[1]?.savedAt || 0) - (a[1]?.savedAt || 0))
      .slice(0, EDITED_CHAPTERS_CACHE_MAX_BOOKS)
    safeLocalSet(EDITED_CHAPTERS_CACHE_KEY, JSON.stringify(Object.fromEntries(trimmed)))
  } catch {
    /* 快照只是离线兜底，写不进去不影响在线阅读 */
  }
}

export const useReaderStore = defineStore('reader', () => {
  type ReaderPanel =
    | 'catalog'
    | 'settings'
    | 'bookshelf'
    | 'source'
    | 'bookmark'
    | 'rule'
    | 'keyword'
    | 'cache'
    | null
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
  /* 常驻关键词高亮：配置存 userdata（keyword-highlights 文档），
     localStorage 是同步镜像；阅读器渲染管线消费它给正文上底色 */
  const keywordRules = ref<KeywordHighlightRule[]>(loadLocalKeywordRules())
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
  /// 已编辑章节的 chapterUrl 集：阅读链路对这些章跳过浏览器缓存直走后端
  /// （后端会优先返回编辑版），目录面板与正文 diff 标记也用它
  const editedChapterKeys = ref<Set<string>>(new Set())
  /// 各章编辑版差异（随 getBookContent 一起返回，不再单独请求）。
  /// 连续滚动模式下每章都从这里取自己的 ops。
  const chapterDiffs = reactive(new Map<number, ChapterDiffState>())
  /// 各章当前版本的定位信息（版本 id、最新上游/用户版本、是否旧数据）。
  const chapterVersions = reactive(new Map<number, ChapterVersionState>())

  async function refreshEditedChapterKeys() {
    // bookId 进函数先快照：请求期间可能换书（loadBook 并行/onMounted 触发），
    // 迟到的旧书结果不能写回，否则会顶掉刚打开的新书集合
    const bookId = book.value?.bookId
    if (!bookId) {
      editedChapterKeys.value = new Set()
      return
    }
    try {
      const urls = await listChapterEdits(bookId)
      if (book.value?.bookId !== bookId) return
      // 先把新集合写回内存再删缓存：集合生效后正文读写才会跳过编辑章；
      // 反过来的话，并发的正文读取会在删完后又把旧上游正文写回 IndexedDB
      editedChapterKeys.value = new Set(urls)
      // 落盘一份供离线打开：离线时拉不到清单，没有快照会把编辑章当普通章节
      saveCachedEditedChapterKeys(bookId, urls)
      // 编辑章的浏览器缓存都是编辑前的上游正文：全部删掉（幂等，批量删除会
      // 按实际存在的记录扣 summary）。带上旧快照里的章节，避免上次删除失败
      // 或升级前留下的记录继续被离线兜底读到
      if (urls.length) {
        await deleteBrowserCachedChapters(bookId, urls).catch(() => undefined)
      }
    } catch {
      /* 离线/失败时保持现状 */
    }
  }
  const progress = createReaderProgress({
    book,
    chapters,
    currentIndex,
    chapterScrollProgress,
    shelfStore,
  })
  const {
    serverProgressTime,
    progressDirty,
    lastServerProgressKey,
    decodeServerProgress,
    markProgressDirty,
    saveReaderSession,
    scheduleReaderSessionSave,
    flushReaderSessionSave,
    syncLocalBookProgress,
    getPersistedReaderSession,
    persistProgress,
    flushProgressToServer,
    flushProgressToServerKeepalive,
  } = progress

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
    // 预载内容是按旧版本抓的：切版本后作废，否则翻到邻章会拿到另一版正文
    if (key === 'contentVersion') {
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
        return scope.slice('source:'.length) === String(currentBook.bookSourceId ?? '')
      }

      if (scope.startsWith('book:')) {
        return scope.slice('book:'.length) === (currentBook.bookId ?? '')
      }

      const scopeParts = scope.split(';')
      if (scopeParts[0] !== '*' && scopeParts[0] !== currentBook.name) {
        return false
      }
      return scopeParts.length === 1 || scopeParts[1] === (currentBook.bookId ?? '')
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

  async function restorePersistedSession() {
    const session = getPersistedReaderSession()
    if (!session?.book) return false

    book.value = session.book
    chapters.value = session.chapters ?? []
    // 瘦身会话（配额降级时落的盘）不带目录：恢复时重新拉一次
    if (!chapters.value.length) {
      const bookId = session.book.bookId
      if (!bookId) return false
      try {
        const list = await getChapterList({ bookId })
        chapters.value = list
        void setBrowserCatalog(bookId, list)
      } catch {
        // 离线兜底：浏览器里缓存的目录
        const cachedCatalog = await getBrowserCatalog<BookChapter>(bookId)
        if (!cachedCatalog) return false
        chapters.value = cachedCatalog
      }
    }
    if (!chapters.value.length) return false
    loadReadChapterHistory(session.book)

    // 先拿到已编辑章清单再取正文：否则恢复会话时会走浏览器缓存，拿到的是
    // 编辑前的上游正文（diff 标记也随之消失）
    await refreshEditedChapterKeys()

    const nextIndex = Math.max(0, Math.min(session.currentIndex || 0, chapters.value.length - 1))
    try {
      const chapterContent = await fetchChapterContent(nextIndex)
      if (chapterContent == null) return false
      setActiveChapterState(nextIndex, chapterContent, session.chapterScrollProgress || 0)
      markChapterAsRead(nextIndex)
      // 恢复会话不经过 loadChapter，评论要在这里单独补一次（配图由 setActive 触发）
      void loadChapterReviews(nextIndex)
      return true
    } catch {
      return false
    }
  }

  // 已读标记本地存储：单 key 文档 { [bookId]: string[] }。
  // 旧版是每书一个 key（前缀 + bookId），书多了污染 localStorage 键空间；
  // 首次访问时把旧键一次性合并迁移进单文档
  const READER_READ_HISTORY_KEY = 'reader-read-history'

  function readReadHistoryDoc(): Record<string, string[]> {
    try {
      const raw = localStorage.getItem(READER_READ_HISTORY_KEY)
      if (!raw) return {}
      const parsed = JSON.parse(raw)
      return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {}
    } catch {
      return {}
    }
  }

  function migrateLegacyReadHistoryKeys() {
    const legacy: Record<string, string[]> = {}
    const staleKeys: string[] = []
    for (let index = 0; index < localStorage.length; index += 1) {
      const key = localStorage.key(index)
      if (!key || !key.startsWith(READER_READ_HISTORY_PREFIX)) continue
      try {
        const parsed = JSON.parse(localStorage.getItem(key) || '[]')
        if (Array.isArray(parsed)) {
          legacy[key.slice(READER_READ_HISTORY_PREFIX.length)] = parsed.filter(
            (item): item is string => typeof item === 'string'
          )
        }
      } catch {
        // 坏数据按空处理，键照样清
      }
      staleKeys.push(key)
    }
    if (!staleKeys.length) return
    const doc = readReadHistoryDoc()
    for (const [bookId, keys] of Object.entries(legacy)) {
      // 与已有分片做并集（新文档可能已被别的书写过）
      doc[bookId] = Array.from(new Set([...(doc[bookId] || []), ...keys]))
    }
    safeLocalSet(READER_READ_HISTORY_KEY, JSON.stringify(doc))
    for (const key of staleKeys) {
      localStorage.removeItem(key)
    }
  }

  function buildReadChapterKey(
    index: number,
    chapter?: BookChapter | null,
    currentBook?: Book | null
  ) {
    if (!currentBook?.bookId) return ''
    // 换源后章节 URL 会变，旧标记自然失效（与旧版按 origin 分段的行为一致）
    if (chapter?.url) {
      return `${currentBook.bookId}::${chapter.url}`
    }
    return `${currentBook.bookId}::index:${index}`
  }

  function loadReadChapterHistory(currentBook?: Book | null) {
    const bookId = currentBook?.bookId
    if (!bookId) {
      readChapterKeys.value = new Set()
      return
    }
    migrateLegacyReadHistoryKeys()
    const doc = readReadHistoryDoc()
    readChapterKeys.value = new Set(Array.isArray(doc[bookId]) ? doc[bookId] : [])
    // 已读标记存在 localStorage，天然单浏览器；与后端文档做并集合并实现跨浏览器同步。
    // 注意本地为空时也必须同步（那正是「另一台浏览器」的场景）
    void syncReadChapterHistoryFromBackend(currentBook)
  }

  function persistReadChapterHistory(currentBook?: Book | null) {
    const bookId = currentBook?.bookId
    if (!bookId) return
    const doc = readReadHistoryDoc()
    doc[bookId] = Array.from(readChapterKeys.value)
    safeLocalSet(READER_READ_HISTORY_KEY, JSON.stringify(doc))
    scheduleReadHistoryPush(currentBook)
  }

  // ─── 已读标记的跨浏览器同步（userdata 单文档：{ [bookId]: string[] }） ───
  const READ_HISTORY_DOC = 'readChapterHistory'

  let readHistoryPushTimer: ReturnType<typeof setTimeout> | null = null
  function scheduleReadHistoryPush(currentBook?: Book | null) {
    const bookId = currentBook?.bookId
    if (!bookId) return
    if (readHistoryPushTimer) clearTimeout(readHistoryPushTimer)
    readHistoryPushTimer = setTimeout(() => {
      readHistoryPushTimer = null
      void pushReadHistory(bookId, Array.from(readChapterKeys.value))
    }, 1500)
  }

  // 读-改-写整文档：只动当前书的分片，别的浏览器写的其他书不受影响
  async function pushReadHistory(bookId: string, keys: string[]) {
    try {
      const doc = (await getUserdata<Record<string, string[]>>(READ_HISTORY_DOC)) || {}
      const remote = new Set(Array.isArray(doc[bookId]) ? doc[bookId] : [])
      const merged = Array.from(new Set([...remote, ...keys]))
      if (merged.length === remote.size) return
      doc[bookId] = merged
      // 推入批量写队列：与其他 userdata 文档合并为一次批量写，失败由队列退避重试
      queueUserdataSave(READ_HISTORY_DOC, doc)
    } catch {
      // 离线/未登录：本地已持久化，下次同步会补上
    }
  }

  async function syncReadChapterHistoryFromBackend(currentBook?: Book | null) {
    const bookId = currentBook?.bookId
    if (!bookId) return
    let doc: Record<string, string[]> | null
    try {
      doc = await getUserdata<Record<string, string[]>>(READ_HISTORY_DOC)
    } catch {
      return // 离线或未登录：保持本地
    }
    // 拉取期间换了书就不要往当前书写
    if (book.value?.bookId !== bookId) return
    const remote = new Set(
      doc && Array.isArray(doc[bookId]) ? doc[bookId].filter((x) => typeof x === 'string') : []
    )
    const merged = new Set([...remote, ...readChapterKeys.value])
    if (merged.size !== readChapterKeys.value.size) {
      readChapterKeys.value = merged
      persistReadChapterHistory(currentBook)
    }
    // 本地多出来的（在别的浏览器里读的）推回后端
    if (merged.size !== remote.size) {
      void pushReadHistory(bookId, Array.from(merged))
    }
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
    // 先用上次成功拉取时落盘的快照：离线开书时清单拉不到，没有快照就会
    // 把编辑章当普通章节读浏览器缓存（编辑前的旧正文）。在线时下面的
    // refreshEditedChapterKeys 会用服务端结果覆盖并更新快照
    editedChapterKeys.value = new Set(b.bookId ? loadCachedEditedChapterKeys(b.bookId) : [])
    // 书源级「不支持评论/配图」结论也按书失效：换书/换源后重新判断
    paraReviewsDisabledBySource = false
    chapterReviewsDisabledBySource = false
    chapterImagesDisabledBySource = false
    appStore.markBookOpened(b.bookId || b.bookUrl)
    currentIndex.value = b.durChapterIndex || 0
    chapterScrollProgress.value = 0
    preloadedContent.value.clear()
    chapterDiffs.clear()
    loadReadChapterHistory(b)
    progressDirty.value = false
    lastServerProgressKey.value = ''
    chaptersLoading.value = true
    try {
      if (!b.bookId) throw new Error('书籍缺少 bookId')
      // 目录与编辑章清单并行；loadBook 返回前两者都要落定，否则开书后的
      // 正文请求可能赶在集合就位前走浏览器缓存/写回旧缓存。
      // refreshEditedChapterKeys 内部已吞异常，不会让 Promise.all reject
      const [list] = await Promise.all([
        getChapterList({ bookId: b.bookId }),
        refreshEditedChapterKeys(),
      ])
      if (loadSeq !== bookLoadSeq) return
      chapters.value = list
      // 目录落浏览器缓存：离线时同一本书还能打开
      void setBrowserCatalog(b.bookId, list)
      saveReaderSession()
    } catch (error) {
      // 目录都拿不到时先试浏览器里的目录缓存（离线场景），
      // 实在没有才等于这本书打不开。清掉半开状态并记下原因：否则阅读页
      // 会一直停在「加载中...」的占位符上，用户完全看不出发生了什么。
      const cachedCatalog = b.bookId ? await getBrowserCatalog<BookChapter>(b.bookId) : null
      if (cachedCatalog && loadSeq === bookLoadSeq) {
        chapters.value = cachedCatalog
        chaptersLoading.value = false
        // 原因要如实：后端在线但上游挂了（502）与浏览器离线是两回事
        appStore.showToast(
          `${describeRequestFailure(error, appStore.isOnline)}：目录来自浏览器缓存`,
          'warning'
        )
        return
      }
      if (loadSeq === bookLoadSeq) {
        loading.value = false
        chapters.value = []
        content.value = ''
        loadError.value = describeRequestFailure(error, appStore.isOnline)
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
    // 配图跟着章节走：先清掉上一章的，再按新正文重新取。
    // 放在这里而不是各个调用方：换章、切正文版本、刷新、连续滚动激活
    // 都会经过这里，漏一处就会少一次配图请求
    resetChapterImages()
    void loadChapterImages(index)
    chapterScrollProgress.value = Math.max(0, Math.min(1, progress))
    if (book.value) {
      book.value.durChapterIndex = index
      book.value.durChapterTitle = chapters.value[index]?.title || book.value.durChapterTitle
      book.value.durChapterTime = Date.now()
      const shelfBook = shelfStore.books.find((item) => item.bookId === book.value?.bookId)
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

  /**
   * 正文落账：版本信息、diff 标记与浏览器缓存写入。单章 fetchChapterContent 与
   * 批量预加载（preloadChaptersBatch）共用这一份，避免两条路径的版本记录与
   * 缓存行为各自漂移。
   */
  async function recordChapterContent(
    index: number,
    chapter: BookChapter,
    response: ChapterContentResponse,
    context: { bookId: string; isLocal: boolean; variant: ContentVariant }
  ) {
    chapterVersions.set(index, {
      versionId: response.versionId,
      kind: response.versionKind,
      baseVersionId: response.baseVersionId,
      latestUserVersionId: response.latestUserVersionId,
      latestUpstreamVersionId: response.latestUpstreamVersionId,
      upstreamChanged: response.upstreamChanged,
      selectionSource: response.selectionSource,
      stale: response.stale,
    })
    if (response.edited) {
      // 后端为省流量不给 equal 行带文本，这里用编辑版正文补齐
      const ops = response.diff?.ops ? hydrateDiffOps(response.diff.ops, response.content) : null
      chapterDiffs.set(index, {
        edited: true,
        ops,
        error: response.diffError || '',
        variant: response.variant,
      })
    } else {
      chapterDiffs.delete(index)
    }

    // 上游预览不写浏览器缓存：缓存只存默认版本，否则离线时会读到另一版。
    // 编辑章同样不写（isEditedChapter），它的最新内容只存在于服务端
    const isEditedChapter = editedChapterKeys.value.has(chapter.url)
    if (!context.isLocal && !isEditedChapter && context.variant !== 'upstream') {
      await setBrowserCachedChapter({
        bookId: context.bookId,
        chapterUrl: chapter.url,
        chapterTitle: chapter.title,
        content: response.content,
      }).catch((error) => {
        // 写不进浏览器缓存（配额写满且无可淘汰）不该静默：阅读功能本身不受影响，
        // 但「缓存永远 0 章」这种症状没有日志就完全无法定位
        console.warn('[reader] 浏览器章节缓存写入失败', error)
      })
    }
  }

  async function fetchChapterContent(index: number, forceRefresh = false) {
    if (!book.value?.bookId || !chapters.value[index]) return null

    if (!forceRefresh && preloadedContent.value.has(index)) {
      return preloadedContent.value.get(index) || null
    }

    const chapter = chapters.value[index]
    const bookId = book.value.bookId

    const isLocal = isLocalBook(book.value)
    const variant: ContentVariant = config.contentVersion === 'upstream' ? 'upstream' : 'default'
    // 在线时总是问后端：手动选择的版本记在服务端，浏览器缓存可能落后于另一台设备。
    // 浏览器缓存只在离线时作为正文来源，以及请求失败时兜底。
    // 编辑章例外：它的浏览器缓存是编辑前的上游正文，离线返回它等于回退到旧内容
    const isEditedChapter = editedChapterKeys.value.has(chapter.url)
    const browserCached =
      !isLocal && !appStore.isOnline && !isEditedChapter
        ? await getBrowserCachedChapter(bookId, chapter.url).catch(() => null)
        : null

    if (!appStore.isOnline && !isLocal) {
      if (browserCached) {
        chapterDiffs.delete(index)
        chapterVersions.delete(index)
        return browserCached
      }
      if (isEditedChapter) {
        throw new Error('本章有编辑版，需要联网读取')
      }
      throw new Error('当前处于离线状态，且该章节未缓存到浏览器')
    }

    let chapterContent = ''
    try {
      const response = await getBookContent({
        bookId,
        chapterUrl: chapter.url,
        index,
        refresh: forceRefresh ? 1 : 0,
        variant,
      })
      chapterContent = response.content
      await recordChapterContent(index, chapter, response, { bookId, isLocal, variant })
    } catch (error) {
      // 在线失败的缓存兜底同样跳过编辑章：理由同上，它的缓存是编辑前的旧正文
      const fallback =
        browserCached ??
        (isLocal || isEditedChapter
          ? null
          : await getBrowserCachedChapter(bookId, chapter.url).catch(() => null))
      if (fallback) {
        chapterDiffs.delete(index)
        chapterVersions.delete(index)
        appStore.showToast(
          `${describeRequestFailure(error, appStore.isOnline)}，已切换到本地缓存章节`,
          'warning'
        )
        return fallback
      }
      throw error
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
      appStore.markChapterRead(
        book.value.bookId || book.value.bookUrl,
        index,
        chapters.value.length
      )

      if (!isOpeningSavedChapter) {
        await persistProgress(index, 0)
      }

      // 正文先渲染，评论随后补上（配图由 setActiveChapterState 统一触发）
      void loadChapterReviews(index)

      if (config.enablePreload) {
        setTimeout(() => preloadAroundChapter(index), forceRefresh ? 1500 : 1000)
      }
    } catch (error) {
      // 失败要留下可见的错误态（正文区展示 + 重试入口），只清 loading 会让用户
      // 对着旧章或空白分不清「还在加载」还是「已经失败」。不再向外抛：所有
      // 调用方（翻章/目录/开书）都不接异常，抛出去只会变成 unhandled rejection。
      if (loadSeq === chapterLoadSeq) {
        loadError.value = describeRequestFailure(error, appStore.isOnline)
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

  /* 书源级「不支持」结论：后端 enabled:false 只取决于书源声明没声明规则，
     与章节无关。同一本书记住后翻章不再重复请求（loadBook 时重置，换书/换源
     重新判断；显式刷新正文也会重新判断）。取舍：书源规则改了，要刷新本章
     或重开这本书才生效，不做自动轮询 */
  let paraReviewsDisabledBySource = false
  let chapterReviewsDisabledBySource = false
  let chapterImagesDisabledBySource = false

  /**
   * 拉取当前章节的段评概览与章评第一页。
   *
   * 书源没声明评论规则时后端返回 `enabled: false`，此时前端不渲染任何入口；
   * 网络或解析失败同样静默降级——评论是附加内容，不能让它影响正文阅读。
   */
  async function loadChapterReviews(index = currentIndex.value, refresh = false) {
    const currentBook = book.value
    const chapter = chapters.value[index]
    if (!currentBook || !chapter || isLocalBook(currentBook)) {
      resetReviews()
      return
    }
    // 只认自己那次请求的结果，避免快速翻章时旧响应覆盖新章节
    const requestedChapterUrl = chapter.url
    const skipParaReviews = !refresh && paraReviewsDisabledBySource
    const skipChapterReviews = !refresh && chapterReviewsDisabledBySource
    // 两条规则都不支持：不再发任何请求，同时清掉上一章残留的评论
    if (skipParaReviews && skipChapterReviews) {
      resetReviews()
      return
    }
    reviewsLoading.value = true
    try {
      if (!currentBook.bookId) throw new Error('书籍缺少 bookId')
      const effectiveKind = chapterVersions.get(index)?.kind
      const params = {
        bookId: currentBook.bookId,
        chapterUrl: requestedChapterUrl,
        // 手动刷新正文时让段评索引也重抓：两者必须来自同一版上游，
        // 否则段号会和刷新后的正文对不上
        refresh: refresh ? 1 : 0,
        // 段评定位看的是实际显示的版本：用户版本与上游版本的段号可能不同
        variant:
          effectiveKind === 'upstream'
            ? 'upstream'
            : effectiveKind
              ? 'edited'
              : config.contentVersion,
      }
      // 段评与章评是两条独立规则：只禁用其一时，另一个仍然请求
      if (skipParaReviews) {
        paraReviewEnabled.value = false
        paraReviewIndex.value = []
      } else {
        // 先取段评概览：后面的章评请求能复用这次取回的章节正文响应
        const indexResp = await getParaCommentIndex(params)
        if (chapters.value[currentIndex.value]?.url !== requestedChapterUrl) return
        paraReviewsDisabledBySource = indexResp.enabled !== true
        paraReviewEnabled.value = indexResp.enabled
        paraReviewIndex.value = indexResp.data?.paras || []
      }
      if (skipChapterReviews) {
        reviewEnabled.value = false
        reviewAuthorMarks.value = false
        chapterCommentTotal.value = 0
        chapterComments.value = null
      } else {
        const chapterResp = await getChapterComments({ ...params, page: 1 })
        if (chapters.value[currentIndex.value]?.url !== requestedChapterUrl) return
        chapterReviewsDisabledBySource = chapterResp.enabled !== true
        reviewEnabled.value = chapterResp.enabled
        reviewAuthorMarks.value = chapterResp.authorMarks === true
        chapterCommentTotal.value = chapterResp.data?.total || 0
        chapterComments.value = chapterResp.data || null
      }
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
    // 书源没有配图规则是书源级结论（见 loadChapterReviews 上方的标记说明）：
    // 记住后本书翻章不再请求，只清掉上一章的配图避免残留在新章上
    if (chapterImagesDisabledBySource) {
      resetChapterImages()
      return
    }
    try {
      if (!currentBook.bookId) throw new Error('书籍缺少 bookId')
      const resp = await getChapterImages({
        bookId: currentBook.bookId,
        chapterUrl: requestedChapterUrl,
      })
      if (chapters.value[currentIndex.value]?.url !== requestedChapterUrl) return
      chapterImagesEnabled.value = resp?.enabled === true
      chapterImages.value = resp?.images || []
      if (resp?.enabled !== true) chapterImagesDisabledBySource = true
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
    // 批量接口一次取回全部目标章（后端上限 10，这里最多 3 条）；整批失败
    // （断网/401/400）时对全部目标章回退逐章预加载，行为与接入批量前一致；
    // 批量成功时只有失败章（单章 error / 响应缺失）逐章兜底——走单章接口
    // 重试并自带浏览器缓存兜底，成功章不重复请求
    const pending = targets.filter((target) => !preloadedContent.value.has(target))
    if (pending.length > 0) {
      const batch = await preloadChaptersBatch(pending)
      if (batch.ok) {
        for (const target of batch.failed) {
          await preloadNextChapter(target)
        }
        return
      }
    }
    for (const target of targets) {
      await preloadNextChapter(target)
    }
  }

  /**
   * 批量预加载：一次 getBookContents 请求取回多章正文写入 preloadedContent。
   * 每章独立成败——`item.data` 为空或响应里缺失的章节记入 failed，由调用方
   * 逐章兜底（单章接口重试 + 浏览器缓存）；ok=false 表示整批失败，由调用方
   * 对全部目标章回退逐章 fetchChapterContent。
   */
  async function preloadChaptersBatch(
    targets: number[]
  ): Promise<{ ok: boolean; failed: number[] }> {
    const currentBook = book.value
    if (!currentBook?.bookId) return { ok: false, failed: [] }
    const bookId = currentBook.bookId
    const isLocal = isLocalBook(currentBook)
    const variant: ContentVariant = config.contentVersion === 'upstream' ? 'upstream' : 'default'
    // 发送时的 index → chapterUrl 快照：响应回来时按它核对章节列表是否已变化
    const sentUrls = new Map<number, string>()
    const chaptersPayload = targets.flatMap((chapterIndex) => {
      const chapter = chapters.value[chapterIndex]
      if (!chapter) return []
      sentUrls.set(chapterIndex, chapter.url)
      return [{ index: chapterIndex, chapterUrl: chapter.url }]
    })
    if (chaptersPayload.length === 0) return { ok: false, failed: [] }

    let items
    try {
      items = await getBookContents({ bookId, chapters: chaptersPayload, variant })
    } catch {
      return { ok: false, failed: [] }
    }
    if (!Array.isArray(items)) return { ok: false, failed: [] }
    // 响应期间换书/换源：旧请求的结果不能写进新书的预加载池（换书已清空
    // preloadedContent，这里直接丢弃，不算失败、也不再回退逐章）
    if (book.value?.bookId !== bookId) return { ok: true, failed: [] }

    // 成功取回正文的请求章；其余（data 为空 / 响应缺失）都要逐章兜底
    const answered = new Set<number>()
    for (const item of items) {
      const chapterIndex = item?.index
      const sentUrl = typeof chapterIndex === 'number' ? sentUrls.get(chapterIndex) : undefined
      if (sentUrl === undefined) continue
      if (!item?.data) continue // 单章失败：原因在 error，记入 failed 走逐章兜底
      answered.add(chapterIndex)
      const chapter = chapters.value[chapterIndex]
      if (!chapter || chapter.url !== sentUrl) continue
      if (preloadedContent.value.has(chapterIndex)) continue
      // 与 preloadNextChapter 同一套容量约束：超出即淘汰最旧的一条
      if (preloadedContent.value.size > 3) {
        const firstKey = preloadedContent.value.keys().next().value
        if (firstKey !== undefined) preloadedContent.value.delete(firstKey)
      }
      preloadedContent.value.set(chapterIndex, item.data.content)
      // 版本信息/diff/浏览器缓存与单章路径同口径落账
      await recordChapterContent(chapterIndex, chapter, item.data, { bookId, isLocal, variant })
    }
    const failed = [...sentUrls.keys()].filter((chapterIndex) => !answered.has(chapterIndex))
    return { ok: true, failed }
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
  /** 换源：目标是候选搜索结果自己的 bookId；当前书的 bookId 不变 */
  async function switchSource(targetBookId: string) {
    if (!book.value?.bookId) return
    const previousChapterTitle = currentChapter.value?.title || book.value.durChapterTitle
    const previousIndex = currentIndex.value
    const previousProgress = chapterScrollProgress.value
    loading.value = true
    try {
      const updatedBook = await apiSetBookSource({
        bookId: book.value.bookId,
        targetBookId,
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

  /** 批量导入后清掉全部章节的预加载与派生状态，后续翻章重新从后端取正文。
   *  只清当前章会让预加载过的相邻章继续显示导入前的旧正文。 */
  async function invalidateChapterContentCache() {
    preloadedContent.value.clear()
    chapterVersions.clear()
    chapterDiffs.clear()
    await refreshEditedChapterKeys()
  }

  /** 编辑版保存/删除后重载本章：清预载与浏览器缓存，重新走后端（编辑版优先），
   *  与 refreshContent 同构但不强刷上游。 */
  async function reloadChapterAfterEdit(index = currentIndex.value) {
    const chapter = chapters.value[index]
    if (!chapter || !book.value?.bookId) return
    preloadedContent.value.delete(index)
    await deleteBrowserCachedChapter(book.value.bookId, chapter.url).catch(() => undefined)
    await refreshEditedChapterKeys()
    const loadSeq = ++chapterLoadSeq
    loading.value = true
    try {
      const chapterContent = await fetchChapterContent(index, false)
      if (chapterContent == null) return
      if (loadSeq !== chapterLoadSeq || index !== currentIndex.value) return
      setActiveChapterState(index, chapterContent, chapterScrollProgress.value)
      // 正文/版本变了，段评概览也要按当前版本重新取（服务端有缓存，代价很小）
      void loadChapterReviews(index)
    } finally {
      loading.value = false
    }
  }

  /** 切换当前章显示版本并保存到服务端（跨设备生效）；`null` 表示跟随最新。 */
  async function setChapterVersion(index: number, versionId: number | null) {
    const chapter = chapters.value[index]
    const bookId = book.value?.bookId
    if (!chapter || !bookId) return
    await selectChapterVersion(bookId, chapter.url, versionId)
    chapterVersions.delete(index)
    preloadedContent.value.clear()
    await refreshEditedChapterKeys()
    if (config.contentVersion === 'upstream') {
      // 在「上游原版」预览里选了版本：切回默认版本模式，ReaderView 的
      // contentVersion watcher 会用新的服务端选择重载本章。
      updateConfig('contentVersion', 'edited')
    } else {
      await reloadChapterAfterEdit(index)
    }
  }

  async function refreshContent() {
    if (!book.value || !chapters.value[currentIndex.value]) return
    // 显式刷新绕过「书源不支持配图」的缓存结论重新判断：书源可能刚补了
    // 规则。reset 在 setActiveChapterState 之前，随后触发的 loadChapterImages
    // 就会真正发请求（评论走 loadChapterReviews(index, true)，自带重判）
    chapterImagesDisabledBySource = false
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
      // 正文基准刚被重抓，段评索引也要跟着刷新
      void loadChapterReviews(index, true)
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
      if (!book.value.bookId) return
      chapters.value = await getChapterList({
        bookId: book.value.bookId,
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
      void setBrowserCatalog(book.value.bookId, chapters.value)
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
      await loadChapter(currentIndex.value + 1)
      if (completedBook && completedChapter) {
        // 自动更新在服务端跑：fire-and-forget，关页面/换书都不受影响
        void aiBookStore.maybeAutoUpdate(completedBook, completedChapter.index)
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

  /* ─── 常驻关键词高亮 ─── */
  /** 管理面板的写入入口：本地镜像 + 防抖推后端（读-改-写整文档） */
  function saveKeywordRules(rules: KeywordHighlightRule[]) {
    keywordRules.value = rules
    persistAndPushKeywordRules(rules)
  }

  /** 拉后端文档合并进本地（进阅读器时调一次；离线静默保持本地） */
  async function syncKeywordRules() {
    keywordRules.value = await syncKeywordRulesFromBackend(keywordRules.value)
  }

  /* ─── Bookmarks ─── */
  async function fetchBookmarks() {
    try {
      const all = await getBookmarks()
      // 只留当前书的（按 bookId 过滤，换源不受影响）
      if (book.value?.bookId) {
        const currentId = book.value.bookId
        bookmarks.value = all.filter((b) => b.bookId === currentId)
      } else {
        bookmarks.value = all
      }
    } catch {
      /* ignore */
    }
  }

  /**
   * pos 是万分比章内进度（0–10000）：章书签恒 0（调用方不传）；
   * 划线传收藏那一刻的章内进度，kind='underline'。
   * 连续模式下划线可能落在非当前章，chapterIndex 由调用方显式覆盖。
   */
  async function addBookmark(
    pos: number = 0,
    snippet: string = '',
    kind: 'bookmark' | 'underline' = 'bookmark',
    chapterIndex?: number
  ) {
    if (!book.value?.bookId) return
    const targetIndex = chapterIndex ?? currentIndex.value
    const chapter = chapters.value[targetIndex]
    if (!chapter) return
    const b: Bookmark = {
      bookId: book.value.bookId,
      bookName: book.value.name,
      bookAuthor: book.value.author,
      chapterIndex: targetIndex,
      chapterName: chapter.title,
      chapterPos: pos,
      bookText: snippet || content.value.slice(0, 50).replace(/<[^>]+>/g, ''),
      time: Date.now(),
      content: '',
      kind,
    }
    await saveBookmark(b)
    await fetchBookmarks()
  }

  async function removeBookmark(b: Bookmark) {
    await apiDeleteBookmark(b)
    await fetchBookmarks()
  }

  /** 更新备注（content 列）：upsert 主键不变，只覆写备注字段 */
  async function updateBookmarkNote(b: Bookmark, note: string) {
    await saveBookmark({ ...b, content: note })
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
    reloadChapterAfterEdit,
    invalidateChapterContentCache,
    setChapterVersion,
    chapterVersions,
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
    updateBookmarkNote,
    removeBookmarks,
    readChapterKeys,
    isChapterRead,
    markChapterAsRead,
    replaceRules,
    fetchReplaceRules,
    keywordRules,
    editedChapterKeys,
    refreshEditedChapterKeys,
    chapterDiffs,
    saveKeywordRules,
    syncKeywordRules,
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
