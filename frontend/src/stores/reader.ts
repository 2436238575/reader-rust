import { defineStore } from 'pinia'
import { safeLocalSet } from '../utils/storage'
import { ref, computed, reactive } from 'vue'
import { useBookshelfStore } from './bookshelf'
import type { Book, BookChapter } from '../types'
import { deleteBrowserCachedChapters } from '../utils/browserCache'
import { saveRecentReadBook } from '../utils/recentBooks'
import { listChapterEdits } from '../api/chapterEdit'
import type { ChapterVersionKind, ContentVariant } from '../api/bookshelf'
import type { ParagraphDiffOp } from '../utils/chapterDiff'
import { useReaderTts } from '../composables/useReaderTts'
import { createReaderProgress } from './readerProgress'
import { saveCachedEditedChapterKeys } from '../utils/editedChaptersCache'

/* ReadConfig 类型与默认配置已随 readerConfig store 迁出（拆分专项） */

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
  const shelfStore = useBookshelfStore()
  const book = ref<Book | null>(null)
  const chapters = ref<BookChapter[]>([])
  const currentIndex = ref(0)
  const content = ref('')
  const loading = ref(false)
  const chaptersLoading = ref(false)
  /// 打开书籍失败的原因。有值时阅读页显示错误而不是「加载中...」占位符
  const loadError = ref('')
  /* 评论（章评/段评）与章节配图在 stores/readerExtras；
     已读标记/书签/关键词高亮在 stores/readerMarks（拆分专项） */
  const preloadedContent = ref<Map<number, string>>(new Map()) // index -> content
  const isAutoScrolling = ref(false)
  const chapterScrollProgress = ref(0)
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

  // 简繁转换器字典的按需加载与缓存已下沉 utils/openccConvert（拆分专项）

  /* 正文呈现管线（替换规则/简繁转换/displayContent）与阅读配置、主题
     已拆到 stores/readerConfig（拆分专项） */

  /* 已读标记（含旧键迁移）已拆到 stores/readerMarks（拆分专项） */

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

  /* 书签与常驻关键词高亮已拆到 stores/readerMarks（拆分专项） */

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
    flushReaderSessionSave,
    getPersistedReaderSession,
    persistProgress,
    flushProgressToServer,
    flushProgressToServerKeepalive,
    toggleAutoReading,
    stopAutoReading,
    activePanel,
    openPanel,
    togglePanel,
    backPanel,
    closePanel,
    editedChapterKeys,
    refreshEditedChapterKeys,
    chapterDiffs,
    ...tts,
    isAutoScrolling,
  }
})
