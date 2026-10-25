import http from './http'
import type { Book, BookChapter, BookGroup, ChapterImages } from '../types'
import { withAppBase } from '../utils/appBase'

export function getBookshelf() {
  return http.get<Book[]>('/getBookshelf').then((r) => r.data)
}

export function getBookshelfWithCacheInfo() {
  return http.get<Book[]>('/getShelfBookWithCacheInfo').then((r) => r.data)
}

export function getShelfBook(bookId: string) {
  return http.post<Book>('/getShelfBook', { bookId }).then((r) => r.data)
}

export function saveBook(book: Partial<Book>) {
  return http.post<Book>('/saveBook', book).then((r) => r.data)
}

export function saveBooks(books: Partial<Book>[]) {
  return http.post<Book[]>('/saveBooks', books).then((r) => r.data)
}

/** WebDAV 恢复：整架覆盖 + 登记表清表重建（备份自带的 bookId 原位恢复） */
export function restoreBooks(books: Partial<Book>[]) {
  return http.post<Book[]>('/saveBooks', { books, restore: true }).then((r) => r.data)
}

export function uploadTxtBook(file: File) {
  const formData = new FormData()
  formData.append('file', file)
  return http
    .post<Book>('/uploadTxtBook', formData, {
      headers: { 'Content-Type': 'multipart/form-data' },
    })
    .then((r) => r.data)
}

export function uploadEpubBook(file: File) {
  const formData = new FormData()
  formData.append('file', file)
  return http
    .post<Book>('/uploadEpubBook', formData, {
      headers: { 'Content-Type': 'multipart/form-data' },
    })
    .then((r) => r.data)
}

export function deleteBook(bookId: string) {
  return http.post<string>('/deleteBook', { bookId }).then((r) => r.data)
}

export function deleteBooks(bookIds: string[]) {
  return http.post<{ deleted: number }>('/deleteBooks', { bookIds }).then((r) => r.data)
}

export function getBookInfo(bookId: string) {
  return http.post<Book>('/getBookInfo', { bookId }).then((r) => r.data)
}

import type { ParagraphDiffOp } from '../utils/chapterDiff'

export type ContentVariant = 'default' | 'upstream'
export type ChapterVersionKind = 'upstream' | 'edited' | 'imported'

/** 章节正文响应：默认版本可能是用户版本，也可能是上游版本。 */
export interface ChapterContentResponse {
  content: string
  /** 当前返回的是否为用户版本（编辑版或手动导入） */
  edited: boolean
  /** `default` 按服务端选择解析；`upstream` 只预览最新上游 */
  variant: ContentVariant
  versionId?: number
  versionKind?: ChapterVersionKind
  /** 用户版本基于哪一版；手动导入未指定时可能为空 */
  baseVersionId?: number
  /** 本次 diff 的基准版本；始终是最新上游版本 */
  diffBaseVersionId?: number
  latestUserVersionId?: number
  latestUpstreamVersionId?: number
  /** 用户版本的基准之后，上游是否又变过 */
  upstreamChanged: boolean
  /** `selected` / `edited` / `upstream` / `upstream_preview` */
  selectionSource: 'selected' | 'edited' | 'upstream' | 'upstream_preview'
  /** 到期未回源，或最近一次回源失败 */
  stale: boolean
  lastAttemptAt?: number
  lastSuccessAt?: number
  upstreamChangedAt?: number
  editedAt?: number
  diff?: { ops: ParagraphDiffOp[] } | null
  /** diff 算不出来的原因（正文仍然可用） */
  diffError?: string | null
}

// ─── 目录列表会话内缓存 ─────────────────────────────────────────────────
// 千章书的目录全量几十到几百 KB，详情弹窗、开书、批量缓存会在短时间内重复拉
// 同一本；这里做 60 秒新鲜期 + 并发合并。refresh=1（阅读器「刷新目录」）绕过；
// 换源与按书清缓存会主动失效对应条目，避免换源后 60 秒内还显示旧源目录。
const CHAPTER_LIST_TTL_MS = 60_000
const CHAPTER_LIST_CACHE_MAX = 20
const chapterListCache = new Map<string, { data: BookChapter[]; expiresAt: number }>()
const chapterListInFlight = new Map<string, Promise<BookChapter[]>>()

function invalidateChapterList(bookId: string | undefined) {
  if (!bookId) return
  chapterListCache.delete(bookId)
  // 在途请求也要摘掉：换源/清缓存后紧接着的 getChapterList 不能复用旧源
  // 还没回来的结果。旧请求的 then 会因身份不再匹配而不写缓存
  chapterListInFlight.delete(bookId)
}

export function getChapterList(params: { bookId: string; refresh?: number }) {
  const { bookId, refresh } = params
  const forceRefresh = refresh === 1
  if (!forceRefresh) {
    const cached = chapterListCache.get(bookId)
    if (cached && cached.expiresAt > Date.now()) {
      return Promise.resolve(cached.data)
    }
    const inFlight = chapterListInFlight.get(bookId)
    if (inFlight) return inFlight
  }
  // GET 而非 POST：响应带强 ETag 时浏览器会自动携 If-None-Match 重验证，
  // 命中 304 直接复用本地缓存；POST 不进 HTTP 缓存，享受不到这层协商
  const request = http
    .get<BookChapter[]>('/getChapterList', { params })
    .then((r) => {
      const data = r.data
      // 只有仍是该书最新一次请求时才写缓存：force 请求可能已抢先写入更新的
      // 结果，慢到的旧请求不应把缓存盖回旧数据。失败不进缓存：回调只在成功时执行
      if (chapterListInFlight.get(bookId) === request) {
        // 上限 20 本，Map 按插入序淘汰最旧，避免会话内无限增长
        chapterListCache.set(bookId, { data, expiresAt: Date.now() + CHAPTER_LIST_TTL_MS })
        if (chapterListCache.size > CHAPTER_LIST_CACHE_MAX) {
          const oldest = chapterListCache.keys().next().value
          if (oldest !== undefined) chapterListCache.delete(oldest)
        }
      }
      return data
    })
    .finally(() => {
      if (chapterListInFlight.get(bookId) === request) {
        chapterListInFlight.delete(bookId)
      }
    })
  chapterListInFlight.set(bookId, request)
  return request
}

export function getBookContent(params: {
  bookId: string
  chapterUrl: string
  index?: number
  refresh?: number
  /** `upstream` 只预览最新上游；缺省/default 按服务端选择解析 */
  variant?: ContentVariant
}) {
  return http.post<ChapterContentResponse>('/getBookContent', params).then((r) => r.data)
}

/** 批量接口里单章的结果：某章失败只写 `error`，不影响其他章节（整体恒 200） */
export interface BatchChapterContentItem {
  index: number
  data: ChapterContentResponse | null
  error: string | null
}

/**
 * 批量读取章节正文（预加载用）：一次最多 10 章，与单章 getBookContent 同一条
 * 读取链路；每章独立成败。不做 refresh：预加载不需要强刷。
 */
export function getBookContents(params: {
  bookId: string
  chapters: { index?: number; chapterUrl: string }[]
  /** `upstream` 只预览最新上游；缺省/default 按服务端选择解析 */
  variant?: ContentVariant
}) {
  return http.post<BatchChapterContentItem[]>('/getBookContents', params).then((r) => r.data)
}

/** 本章配图；书源没声明配图规则时返回 `enabled: false` */
export function getChapterImages(params: { bookId: string; chapterUrl: string }) {
  return http.post<ChapterImages>('/getChapterImages', params).then((r) => r.data)
}

export function saveBookProgress(params: { bookId: string; index: number; position?: number }) {
  return http.post<string>('/saveBookProgress', params).then((r) => r.data)
}

export function deleteBookCache(bookId: string) {
  return http.post('/deleteBookCache', { bookId }).then((r) => {
    // 服务端已按书清掉目录等可再生缓存：本地目录缓存一并失效，
    // 避免 60 秒新鲜期内还返回清缓存前的旧目录
    invalidateChapterList(bookId)
    return r.data
  })
}

// ─── Groups ───
export function getBookGroups() {
  return http.get<BookGroup[]>('/getBookGroups').then((r) => r.data)
}

export function saveBookGroup(group: BookGroup) {
  return http.post<string>('/saveBookGroup', group).then((r) => r.data)
}

export function deleteBookGroup(groupId: number) {
  return http.post<string>('/deleteBookGroup', { groupId }).then((r) => r.data)
}

export function saveBookGroupId(bookId: string, groupId: number) {
  return http.post<string>('/saveBookGroupId', { bookId, groupId }).then((r) => r.data)
}

/** 换源：目标是换源候选（搜索结果）自己的 bookId，当前书的 bookId 不变 */
export function setBookSource(params: { bookId: string; targetBookId: string }) {
  return http.post<Book>('/setBookSource', params).then((r) => {
    // 换源只改 bookId 的书源绑定，目录会整体变化：立即失效本地目录缓存，
    // 详情弹窗换源后的重拉（不带 refresh）也能拿到新源目录
    invalidateChapterList(params.bookId)
    return r.data
  })
}

// ─── Cover helper ───
export function getCoverUrl(coverUrl?: string) {
  if (!coverUrl) return ''
  if (coverUrl.startsWith('/reader3/localEpubAsset')) {
    return withAppBase(coverUrl)
  }
  // 后端已经把书源地址换成了本站取图地址（`/reader3/image/<id>`）：
  // id 由后端映射表维护，前端不接触会过期的书源地址，会话 Cookie 随 <img> 自动携带
  if (coverUrl.startsWith('/reader3/image/')) {
    return withAppBase(coverUrl)
  }
  if (coverUrl.startsWith('http') || coverUrl.startsWith('/')) {
    // 兼容入口：customCoverUrl 是用户自设的任意地址，走封面接口代理
    const params = new URLSearchParams({ path: coverUrl })
    return withAppBase(`/reader3/cover?${params.toString()}`)
  }
  return coverUrl
}

/// 给需要鉴权的直链（本地书资源、封面、配图等）套上部署前缀。
/// 会话凭据由 HttpOnly Cookie 承载，<img>/直链请求浏览器自动携带，URL 不再
/// 拼任何令牌；完整的 http(s) 地址指向第三方主机，原样返回（约定上后端已把
/// 书源图片收口为 /reader3/image/<id>，走到这里的外源地址是兜底场景，
/// 宁可不渲染）。
export function withAuthQuery(url: string) {
  if (/^https?:\/\//i.test(url)) return url
  return withAppBase(url)
}
