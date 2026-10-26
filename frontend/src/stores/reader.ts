import { defineStore } from 'pinia'
import { safeLocalSet } from '../utils/storage'
import { ref, computed, reactive, watch } from 'vue'
import { useAppStore } from './app'
import { useBookshelfStore } from './bookshelf'
import {
  getBookmarks,
  saveBookmark,
  deleteBookmark as apiDeleteBookmark,
  deleteBookmarks as apiDeleteBookmarks,
} from '../api/bookmark'
import { getUserdata } from '../api/userdata'
import { queueUserdataSave } from '../utils/userdataQueue'
import { READ_HISTORY } from '../utils/userdataDocs'
import { getReplaceRules } from '../api/replaceRule'
import type { Book, BookChapter, Bookmark, ReplaceRule } from '../types'
import { deleteBrowserCachedChapters } from '../utils/browserCache'
import { saveRecentReadBook } from '../utils/recentBooks'
import { listChapterEdits } from '../api/chapterEdit'
import type { ChapterVersionKind, ContentVariant } from '../api/bookshelf'
import type { ParagraphDiffOp } from '../utils/chapterDiff'
import {
  loadLocalKeywordRules,
  persistAndPushKeywordRules,
  syncKeywordRulesFromBackend,
  type KeywordHighlightRule,
} from '../utils/keywordHighlights'
import { useReaderTts } from '../composables/useReaderTts'
import { createReaderProgress } from './readerProgress'
import { fontPresets, themePresets, type ThemePreset } from '../utils/readerTheme'
import { convertChineseText, ensureChineseConverter } from '../utils/openccConvert'
import { applyReplaceRulesToText } from '../utils/replaceRules'
import { saveCachedEditedChapterKeys } from '../utils/editedChaptersCache'

// 常量已下沉 utils/readerTheme（拆分专项）；这里再导出只是兼容旧 import 路径
export { fontPresets, themePresets }
export type { ThemePreset }

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
  /* 评论（章评/段评）与章节配图已拆到 stores/readerExtras（拆分专项） */
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

  // 简繁转换器字典的按需加载与缓存已下沉 utils/openccConvert（拆分专项）

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
  /* scope 匹配与按序应用的纯逻辑在 utils/replaceRules（拆分专项） */
  function applyReplaceRules(text: string) {
    return applyReplaceRulesToText(text, replaceRules.value, book.value)
  }

  function convertContent(text: string) {
    return convertChineseText(text, config.chineseMode)
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
        void ensureChineseConverter('traditional')
      }
    },
    { immediate: true }
  )

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
  const READ_HISTORY_DOC = READ_HISTORY

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
  // 开书/正文管线（loadBook/loadChapter/fetchChapterContent/预加载/换源/刷新…）
  // 已拆到 stores/readerContent；核心只保留状态与本文件内的落账入口。

  /* 章节激活信号：每次 setActiveChapterState 落定后 +1。
     配图等「跟着激活走」的附加内容由 readerContent 监听它触发（等价于拆分前
     在这里直接调 loadChapterImages）；用计数器而不是 watch currentIndex/content：
     连续两章正文相同（纯图源章正文皆空）或同章刷新时索引/正文都可能不变，
     只有激活计数能精确表达「这一章被重新落定」这一事件 */
  const chapterActivation = ref(0)

  function setActiveChapterState(index: number, chapterContent: string, progress = 0) {
    currentIndex.value = index
    content.value = chapterContent
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
    // 落定后广播激活：配图等附加内容由 readerContent 监听此信号触发
    chapterActivation.value += 1
  }

  function setChapterScrollProgress(value: number) {
    chapterScrollProgress.value = Math.max(0, Math.min(1, value))
    markProgressDirty()
    scheduleReaderSessionSave()
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
    progressDirty,
    lastServerProgressKey,
    currentChapter,
    hasNext,
    hasPrev,
    readingProgress,
    setActiveChapterState,
    chapterActivation,
    chapterVersions,
    chapterScrollProgress,
    setChapterScrollProgress,
    preloadedContent,
    decodeServerProgress,
    saveReaderSession,
    loadReadChapterHistory,
    flushReaderSessionSave,
    getPersistedReaderSession,
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
    ...tts,
    displayContent,
    processContentForDisplay,
    isAutoScrolling,
  }
})
