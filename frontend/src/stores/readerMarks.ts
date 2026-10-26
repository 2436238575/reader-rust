// 已读标记、书签与常驻关键词高亮：从 reader store 拆出的子 store（reader store 拆分专项）。
//
// 依赖方向：本 store 依赖 reader 核心（stores/reader）；reader 不反向依赖本 store。
// 触发接线：开书/恢复会话后的历史加载与翻章后的打标由 readerContent 显式调用
// （loadReadChapterHistory / markChapterAsRead），核心不再持有这些入口。
//
// 已编辑章集合（editedChapterKeys）不在此：正文管线（fetchChapterContent 的缓存
// 跳过判断）直接读它，留在 reader 核心；其 localStorage 快照纯函数在
// utils/editedChaptersCache。

import { defineStore } from 'pinia'
import { ref } from 'vue'
import { useReaderStore } from './reader'
import { safeLocalSet } from '../utils/storage'
import {
  getBookmarks,
  saveBookmark,
  deleteBookmark as apiDeleteBookmark,
  deleteBookmarks as apiDeleteBookmarks,
} from '../api/bookmark'
import { getUserdata } from '../api/userdata'
import { queueUserdataSave } from '../utils/userdataQueue'
import { READ_HISTORY } from '../utils/userdataDocs'
import type { Book, Bookmark } from '../types'
import {
  loadLocalKeywordRules,
  persistAndPushKeywordRules,
  syncKeywordRulesFromBackend,
  type KeywordHighlightRule,
} from '../utils/keywordHighlights'

export const useReaderMarksStore = defineStore('readerMarks', () => {
  const readerStore = useReaderStore()

  const readChapterKeys = ref<Set<string>>(new Set())
  const bookmarks = ref<Bookmark[]>([])
  /* 常驻关键词高亮：配置存 userdata（keyword-highlights 文档），
     localStorage 是同步镜像；阅读器渲染管线消费它给正文上底色 */
  const keywordRules = ref<KeywordHighlightRule[]>(loadLocalKeywordRules())

  // 已读标记本地存储：单 key 文档 { [bookId]: string[] }
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

  function buildReadChapterKey(
    index: number,
    chapter?: { url?: string } | null,
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
    if (readerStore.book?.bookId !== bookId) return
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
    const key = buildReadChapterKey(index, readerStore.chapters[index], readerStore.book)
    if (!key || readChapterKeys.value.has(key)) return
    const next = new Set(readChapterKeys.value)
    next.add(key)
    readChapterKeys.value = next
    persistReadChapterHistory(readerStore.book)
  }

  function isChapterRead(index: number) {
    return readChapterKeys.value.has(
      buildReadChapterKey(index, readerStore.chapters[index], readerStore.book)
    )
  }

  /* ─── Bookmarks ─── */
  async function fetchBookmarks() {
    try {
      const all = await getBookmarks()
      // 只留当前书的（按 bookId 过滤，换源不受影响）
      if (readerStore.book?.bookId) {
        const currentId = readerStore.book.bookId
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
    if (!readerStore.book?.bookId) return
    const targetIndex = chapterIndex ?? readerStore.currentIndex
    const chapter = readerStore.chapters[targetIndex]
    if (!chapter) return
    const b: Bookmark = {
      bookId: readerStore.book.bookId,
      bookName: readerStore.book.name,
      bookAuthor: readerStore.book.author,
      chapterIndex: targetIndex,
      chapterName: chapter.title,
      chapterPos: pos,
      bookText: snippet || readerStore.content.slice(0, 50).replace(/<[^>]+>/g, ''),
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

  return {
    readChapterKeys,
    isChapterRead,
    markChapterAsRead,
    loadReadChapterHistory,
    bookmarks,
    fetchBookmarks,
    addBookmark,
    removeBookmark,
    updateBookmarkNote,
    removeBookmarks,
    keywordRules,
    saveKeywordRules,
    syncKeywordRules,
  }
})
