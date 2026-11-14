import http, { UPLOAD_TIMEOUT_MS } from './http'
import type { Book, BookChapter, BookGroup, ChapterImages, ShelfBook } from '../types'
import { API_V1_BASE, withAppBase } from '../utils/appBase'
import type { components } from './generated'
import { parseSourceId } from './source'

// v1 生成的具名 schema（契约 §3.1）
type ApiChapterItem = components['schemas']['ChapterItem']
type ApiChapterPage = components['schemas']['ChapterPage']
type ApiContent = components['schemas']['ChapterContent']
type ApiBatchContent = components['schemas']['BatchContentItem']
type ApiShelfItem = components['schemas']['ShelfItem']
type ApiShelfPage = components['schemas']['ShelfPage']

/** `cv_<n>` → 数字版本号（旧响应形状消费方仍用数字；解析失败按缺省处理） */
function parseVersionId(id: string | null | undefined): number | undefined {
  if (!id) return undefined
  const parsed = Number(id.replace(/^cv_/, ''))
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : undefined
}

/** RFC 3339 → 秒级时间戳（旧响应形状里时间是数字秒） */
function parseSeconds(iso: string | null | undefined): number | undefined {
  if (!iso) return undefined
  const ms = Date.parse(iso)
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : undefined
}

function mapChapterItem(item: ApiChapterItem): BookChapter {
  return {
    index: item.index,
    title: item.title,
    url: item.url,
    isVip: item.isVip,
    isPay: item.isPay,
    isVolume: item.isVolume,
    volume: item.volume,
    wordCount: item.wordCount,
  }
}

/** v1 游标分页拉全量目录（阅读器需要整本章节，200/页，百页以内）。 */
async function fetchAllChaptersV1(
  bookId: string,
  refresh: boolean
): Promise<{ chapters: BookChapter[]; stale: boolean; complete: boolean }> {
  const fetchOnce = async (allowRefresh: boolean) => {
    const chapters: BookChapter[] = []
    let cursor: string | null = null
    let stale = false
    let complete = true
    // 第一页的目录快照令牌：续页回传，服务端发现目录已被后台刷新时回 409
    let tocVersion: number | null = null
    let firstPage = true
    let needRefresh = allowRefresh
    do {
      const params: Record<string, unknown> = { limit: 200 }
      if (cursor) {
        params.cursor = cursor
        if (tocVersion !== null) params.tocVersion = tocVersion
      }
      // refresh 只随第一页发送：后端同步重抓后整表替换，续页沿用新目录
      if (needRefresh && firstPage) params.refresh = true
      const page: ApiChapterPage = await http
        .get<ApiChapterPage>(`/books/${bookId}/chapters`, {
          baseURL: API_V1_BASE,
          params,
          timeout: CHAPTER_LIST_TIMEOUT_MS,
        })
        .then((r) => r.data)
      for (const item of page.items) chapters.push(mapChapterItem(item))
      stale = stale || Boolean(page.stale)
      complete = complete && page.complete !== false
      if (firstPage) tocVersion = page.tocVersion ?? null
      firstPage = false
      needRefresh = false
      cursor = page.nextCursor ?? null
    } while (cursor)
    return { chapters, stale, complete }
  }
  try {
    return await fetchOnce(refresh)
  } catch (error) {
    // 翻页途中目录被后台刷新整表替换（index 可能重排）：从第一页重拉一次。
    // 重拉不再带 refresh——快照已经换新，不需要再回源。
    if ((error as { code?: string }).code === 'STALE_CURSOR') {
      return fetchOnce(false)
    }
    throw error
  }
}

function mapContentToLegacy(data: ApiContent, variant?: ContentVariant): ChapterContentResponse {
  return {
    content: data.body,
    edited: data.edited,
    variant: data.selectionSource === 'upstream_preview' ? 'upstream' : (variant ?? 'default'),
    versionId: parseVersionId(data.version?.id),
    versionKind: data.version?.kind as ChapterVersionKind | undefined,
    baseVersionId: parseVersionId(data.version?.basedOn),
    diffBaseVersionId: parseVersionId(data.diffBaseVersionId),
    latestUserVersionId: parseVersionId(data.latestUserVersionId),
    latestUpstreamVersionId: parseVersionId(data.latestUpstreamVersionId),
    upstreamChanged: data.upstreamChangedSinceBase,
    selectionSource: data.selectionSource as ChapterContentResponse['selectionSource'],
    stale: data.stale,
    lastAttemptAt: parseSeconds(data.lastAttemptAt),
    lastSuccessAt: parseSeconds(data.fetchedAt),
    upstreamChangedAt: parseSeconds(data.upstreamChangedAt),
    editedAt: parseSeconds(data.editedAt),
    diff: data.diff ? { ops: data.diff as ParagraphDiffOp[] } : null,
    diffError: data.diffError ?? null,
    unchanged: false,
  }
}

/** v1 ShelfItem → 旧 Book 形状（store/组件仍在消费平铺字段）。 */
function mapShelfItem(item: ApiShelfItem): ShelfBook {
  return {
    bookId: item.bookId,
    bookSourceId: item.sourceId ? parseSourceId(item.sourceId) : undefined,
    name: item.name,
    author: item.author,
    bookUrl: item.bookUrl,
    origin: item.origin,
    originName: item.originName ?? undefined,
    // 后端只给图片管道 id 或站内资源地址（本地 EPUB 封面）；
    // 本站取图地址仍走 /reader3/image/<id>（图片域未迁）
    coverUrl:
      item.coverUrl ??
      (item.coverImageId ? `/reader3/image/${item.coverImageId}` : undefined),
    customCoverUrl: item.customCoverUrl ?? undefined,
    intro: item.intro ?? undefined,
    kind: item.kind ?? undefined,
    charset: item.charset ?? undefined,
    canUpdate: item.canUpdate ?? undefined,
    group: item.group,
    durChapterIndex: item.progress.chapterIndex,
    durChapterPos: Math.round((item.progress.offset ?? 0) * 10000),
    durChapterTitle: item.progress.chapterTitle ?? undefined,
    durChapterTime: parseSeconds(item.lastReadAt),
    latestChapterTitle: item.latestChapterTitle ?? undefined,
    totalChapterNum: item.totalChapters ?? undefined,
    wordCount: item.wordCount ?? undefined,
    lastCheckTime: parseSeconds(item.lastCheckAt),
    cachedChapterCount: item.cachedChapters,
  }
}

/** v1 游标分页拉全量书架（阅读器/书架页需要整架数据，200/页）。 */
async function fetchAllShelfV1(): Promise<ShelfBook[]> {
  const books: ShelfBook[] = []
  let cursor: string | null = null
  do {
    const params: Record<string, unknown> = { limit: 200 }
    if (cursor) params.cursor = cursor
    const page: ApiShelfPage = await http
      .get<ApiShelfPage>('/me/shelf', { baseURL: API_V1_BASE, params })
      .then((r) => r.data)
    for (const item of page.items) books.push(mapShelfItem(item))
    cursor = page.nextCursor ?? null
  } while (cursor)
  return books
}

/**
 * 书架列表（带服务端缓存章数）。仍保留 WebDAV 备份专用 `getBookshelf`
 * 走 `/reader3`：备份快照要完整 Book（tocUrl/infoHtml 等在 v1 书架条目里
 * 不暴露），备份域切换时一并迁移。
 */
export function getBookshelf() {
  return http.get<Book[]>('/getBookshelf').then((r) => r.data)
}

export function getBookshelfWithCacheInfo() {
  return fetchAllShelfV1()
}

export function getShelfBook(bookId: string) {
  return http
    .get<ApiShelfItem>(`/me/shelf/${bookId}`, { baseURL: API_V1_BASE })
    .then((r) => mapShelfItem(r.data))
}

/**
 * 入架（v1，幂等）：201 新建 / 200 已在架。已在架时不重置进度与元数据，
 * 前端据此决定本地是追加还是原样保留。
 *
 * `display` 是调用方（搜索/发现结果）手里的展示字段：带上后服务端跳过
 * 回源详情，发现页直接入架不再等最坏 3×10s 的详情重试。
 */
export function addToShelf(
  bookId: string,
  display?: {
    name?: string
    author?: string
    coverUrl?: string
    intro?: string
    kind?: string
  }
) {
  const body = display
    ? {
        ...(display.name ? { name: display.name } : {}),
        ...(display.author ? { author: display.author } : {}),
        ...(display.coverUrl ? { coverUrl: display.coverUrl } : {}),
        ...(display.intro ? { intro: display.intro } : {}),
        ...(display.kind ? { kind: display.kind } : {}),
      }
    : {}
  return http
    .put<{ bookId?: string }>(`/me/shelf/${bookId}`, body, {
      baseURL: API_V1_BASE,
    })
    .then((r) => ({
      bookId: r.data?.bookId ?? bookId,
      created: r.status === 201,
    }))
}

/** 批量移出的分片大小：服务端单批上限 1000，按 500 串行分片，
 *  全选超大书架也不会撞 400。 */
const SHELF_BATCH_DELETE_CHUNK = 500

/**
 * 批量移出（v1）：与单个 DELETE 同语义（登记行/书签/正文历史保留），
 * 不存在的 id 忽略。超过单批上限时按 500 分片串行发送。
 */
export async function batchRemoveFromShelf(bookIds: string[]) {
  let deleted = 0
  for (let i = 0; i < bookIds.length; i += SHELF_BATCH_DELETE_CHUNK) {
    const result = await http
      .post<{ deleted: number }>(
        '/me/shelf:batchDelete',
        { bookIds: bookIds.slice(i, i + SHELF_BATCH_DELETE_CHUNK) },
        { baseURL: API_V1_BASE }
      )
      .then((r) => r.data)
    deleted += result.deleted
  }
  return { deleted }
}

/**
 * 移出书架（v1，幂等 204）：登记行、书签与正文历史保留，重新加入接得上。
 * 与旧 `/reader3/deleteBook` 的区别：不再顺带删书签/AI 记忆/图片/本地书文件。
 */
export function removeFromShelf(bookId: string) {
  return http
    .delete(`/me/shelf/${bookId}`, { baseURL: API_V1_BASE })
    .then((r) => r.data)
}

export function saveBooks(books: Partial<Book>[]) {
  return http.post<Book[]>('/saveBooks', books).then((r) => r.data)
}

/**
 * 上传本地书（v1）：TXT/EPUB 按扩展名分派（`.epub` 走 EPUB，其余按 TXT），
 * 响应是不带包装的 Book（已分配 bookId 且已在书架）。
 */
export function uploadLocalBook(file: File) {
  const formData = new FormData()
  formData.append('file', file)
  return http
    .post<Book>('/local-books', formData, {
      headers: { 'Content-Type': 'multipart/form-data' },
      timeout: UPLOAD_TIMEOUT_MS,
      baseURL: API_V1_BASE,
    })
    .then((r) => r.data)
}

export function uploadTxtBook(file: File) {
  return uploadLocalBook(file)
}

export function uploadEpubBook(file: File) {
  return uploadLocalBook(file)
}

export function deleteBooks(bookIds: string[]) {
  return http.post<{ deleted: number }>('/deleteBooks', { bookIds }).then((r) => r.data)
}

/**
 * v1 书籍详情映射成的覆盖层：字段都是「服务端确实给出」的，调用方按
 * `{ ...store.book, ...latest }` 合并。`origin`/`originName` v1 不提供，
 * 由书架条目保留——绝不能在这里补空串。
 */
export type BookInfoOverlay = {
  bookId: string
  name: string
  bookUrl: string
} & Partial<Book>

/**
 * 获取书籍详情（v1）：ReaderSource 换源预览用。
 * 返回覆盖层（见 `BookInfoOverlay`）：网络书封面走 `coverImageId`，
 * 本地 EPUB 封面原样给 `coverUrl`。
 */
export function getBookInfo(bookId: string): Promise<BookInfoOverlay> {
  return http
    .get<{
      id: string
      name: string
      author?: string | null
      sourceId?: string | null
      bookUrl: string
      intro?: string | null
      kind?: string | null
      coverImageId?: string | null
      /** 站内资源地址（本地 EPUB 封面）；网络书封面只给 coverImageId */
      coverUrl?: string | null
      totalChapters?: number | null
      lastChapter?: { index: number; title: string } | null
      stale: boolean
    }>(`/books/${bookId}`, { baseURL: API_V1_BASE })
    .then((r) => {
      const d = r.data
      // 只覆盖服务端确实给出的字段：详情弹窗按 `{ ...store.book, ...latest }`
      // 合并，这里塞 `undefined`/空串会把书架条目的真值（尤其 origin/originName）
      // 抹掉——v1 BookDetail 本来就没有这两个字段，合并时必须保留旧值。
      const book: BookInfoOverlay = {
        bookId: d.id,
        name: d.name,
        bookUrl: d.bookUrl,
        ...(d.sourceId
          ? { bookSourceId: Number(d.sourceId.replace(/^src_/, '')) }
          : {}),
        ...(d.author ? { author: d.author } : {}),
        ...(d.intro ? { intro: d.intro } : {}),
        ...(d.kind ? { kind: d.kind } : {}),
        ...(d.coverUrl
          ? { coverUrl: d.coverUrl }
          : d.coverImageId
            ? { coverUrl: `/reader3/image/${d.coverImageId}` }
            : {}),
        ...(d.lastChapter?.title
          ? { latestChapterTitle: d.lastChapter.title }
          : {}),
        ...(d.totalChapters != null
          ? { totalChapterNum: d.totalChapters }
          : {}),
      }
      return book
    })
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
  /** 浏览器缓存的版本仍是最新：正文未传（content 为空串），前端用缓存正文；
   *  版本信息/diff 等其余字段照常返回 */
  unchanged: boolean
}

// ─── 目录列表会话内缓存 ─────────────────────────────────────────────────
// 千章书的目录全量几十到几百 KB，详情弹窗、开书、批量缓存会在短时间内重复拉
// 同一本；这里做 60 秒新鲜期 + 并发合并。refresh=1（阅读器「刷新目录」）绕过；
// 换源与按书清缓存会主动失效对应条目，避免换源后 60 秒内还显示旧源目录。
const CHAPTER_LIST_TTL_MS = 60_000
const CHAPTER_LIST_CACHE_MAX = 20
// 目录请求单独收敛超时（不沿用 http 实例的 120s 默认）：后端目录链路最坏是三跳
// 串行（analyze → fetch 15s → loginCheck 后再 fetch 15s → 可能再 15s），上游挂掉时
// 120s 等于开书干等两分钟才落到浏览器缓存兜底。健康上游各跳远小于 15s，双跳都吃满
// 已等于实质死亡，20s 切断交给调用方走既有的缓存兜底与提示。
// ETag 304 复用路径不受影响：条件请求命中时后端即时应答 304（不触发上游抓取），
// 超时只在服务端完全不给响应字节时才生效。
const CHAPTER_LIST_TIMEOUT_MS = 20_000
// 服务端返回 stale=true 表示登记表已过 TTL、后台正在刷新：会话缓存改用
// 短 TTL，让下一次目录读取（开书/翻目录）尽快拿到刷新后的结果
const STALE_CHAPTER_LIST_TTL_MS = 10_000
const chapterListCache = new Map<string, { data: BookChapter[]; expiresAt: number }>()
const chapterListInFlight = new Map<string, Promise<BookChapter[]>>()

/** 失效目录会话缓存（改名等标题变化后调用，让下一次拉取绕过 60 秒新鲜期）。 */
export function invalidateChapterList(bookId: string | undefined) {
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
  // v1：游标分页拉全量；响应带 ETag + Cache-Control: no-cache，浏览器对
  // 同一 URL 自动携 If-None-Match 重验证，命中 304 直接复用本地响应体
  const request = fetchAllChaptersV1(bookId, forceRefresh)
    .then(({ chapters, stale, complete }) => {
      // 只有仍是该书最新一次请求时才写缓存：force 请求可能已抢先写入更新的
      // 结果，慢到的旧请求不应把缓存盖回旧数据。失败不进缓存：回调只在成功时执行
      if (chapterListInFlight.get(bookId) === request) {
        // 上限 20 本，Map 按插入序淘汰最旧，避免会话内无限增长。
        // 过期（后台刷新中）或未抓全的目录用短 TTL，尽快换到补全后的结果
        chapterListCache.set(bookId, {
          data: chapters,
          expiresAt:
            Date.now() +
            (stale || !complete ? STALE_CHAPTER_LIST_TTL_MS : CHAPTER_LIST_TTL_MS),
        })
        if (chapterListCache.size > CHAPTER_LIST_CACHE_MAX) {
          const oldest = chapterListCache.keys().next().value
          if (oldest !== undefined) chapterListCache.delete(oldest)
        }
      }
      return chapters
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
  /**
   * 旧协议的本地缓存版本号。v1 不再用它做服务端「unchanged」比对——
   * 条件请求交给浏览器 HTTP 缓存（响应带 ETag + no-cache，客户端无需管理），
   * 因此这里忽略该参数，保留签名只为调用方零改动。
   */
  knownVersionId?: number
}) {
  if (typeof params.index !== 'number') {
    return Promise.reject(new Error('章节缺少 index（v1 以 index 定位章节）'))
  }
  const query: Record<string, unknown> = {}
  if (params.refresh === 1) query.refresh = true
  if (params.variant === 'upstream') query.variant = 'upstream'
  return http
    .get<ApiContent>(`/books/${params.bookId}/chapters/${params.index}/content`, {
      baseURL: API_V1_BASE,
      params: query,
    })
    .then((r) => mapContentToLegacy(r.data, params.variant))
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
 *
 * 生产调用方：阅读器预加载（`readerContent.preloadChaptersBatch`）。
 * 整本「缓存到浏览器」（`bookCache.cacheBookToBrowser`）仍是逐章请求，
 * 批量接入待做。
 */
export async function getBookContents(params: {
  bookId: string
  chapters: { index?: number; chapterUrl: string; knownVersionId?: number }[]
  /** `upstream` 只预览最新上游；缺省/default 按服务端选择解析 */
  variant?: ContentVariant
}): Promise<BatchChapterContentItem[]> {
  // v1 以 index 定位；没有 index 的条目（不应出现）直接跳过，
  // 调用方按「响应缺失」走逐章兜底
  const indices: number[] = []
  const knownVersionById = new Map<number, number | undefined>()
  for (const chapter of params.chapters) {
    if (typeof chapter.index !== 'number' || indices.includes(chapter.index)) continue
    indices.push(chapter.index)
    knownVersionById.set(chapter.index, chapter.knownVersionId)
  }
  if (indices.length === 0) return []
  const query: Record<string, unknown> = {}
  if (params.variant === 'upstream') query.variant = 'upstream'
  const response = await http
    .post<{ items: ApiBatchContent[] }>(
      `/books/${params.bookId}/chapters:batchContent`,
      { indices },
      { baseURL: API_V1_BASE, params: query }
    )
    .then((r) => r.data)
  const byIndex = new Map<number, ApiBatchContent>()
  for (const item of response.items) byIndex.set(item.index, item)
  return indices.map((index) => {
    const item = byIndex.get(index)
    if (!item?.data) {
      const error = item?.error
      return {
        index,
        data: null,
        error: error ? `${error.detail ?? ''}`.trim() || String(error.code) : '批次缺失',
      }
    }
    const data = mapContentToLegacy(item.data, params.variant)
    // 批量接口不带条件请求：本地版本号一致时在客户端等价「unchanged」，
    // 下游沿用「用浏览器缓存正文」的既有语义（不发多余写库）
    const known = knownVersionById.get(index)
    if (known !== undefined && data.versionId === known) {
      return { index, data: { ...data, content: '', unchanged: true }, error: null }
    }
    return { index, data, error: null }
  })
}

/** 本章配图；书源没声明配图规则时返回 `enabled: false` */
/** 本章配图；书源没声明配图规则时返回 `enabled: false` */
export function getChapterImages(params: { bookId: string; index: number }) {
  return http
    .get<ChapterImages>(`/books/${params.bookId}/chapters/${params.index}/images`, {
      baseURL: API_V1_BASE,
    })
    .then((r) => r.data)
}

export function saveBookProgress(params: { bookId: string; index: number; position?: number }) {
  // 旧口诀：position 是万分比章内进度；v1 用 0..1 比例
  const offset =
    typeof params.position === 'number'
      ? Math.min(1, Math.max(0, params.position / 10000))
      : undefined
  const body = {
    progress: { chapterIndex: params.index, ...(offset !== undefined ? { offset } : {}) },
  }
  return http
    .patch(`/me/shelf/${params.bookId}`, body, { baseURL: API_V1_BASE })
    .then((r) => r.data)
}

export function deleteBookCache(bookId: string) {
  // v1：书籍级缓存清理 = 重置目录抓取状态 + 删换源候选（登记行与正文历史不动）
  return http.delete(`/books/${bookId}/cache`, { baseURL: API_V1_BASE }).then((r) => {
    // 服务端已按书清掉目录等可再生缓存：本地目录缓存一并失效，
    // 避免 60 秒新鲜期内还返回清缓存前的旧目录
    invalidateChapterList(bookId)
    return r.data
  })
}

// ─── Groups ───
export function getBookGroups() {
  return http.get<BookGroup[]>('/me/groups', { baseURL: API_V1_BASE }).then((r) => r.data)
}

/** 新建（groupId=0，服务端分配空位幂）或更新 */
export function saveBookGroup(group: BookGroup) {
  return http.post('/me/groups', group, { baseURL: API_V1_BASE }).then(() => '')
}

/** 删组：组行与书籍位掩码里的对应位一起清 */
export function deleteBookGroup(groupId: number) {
  return http
    .delete(`/me/groups/${groupId}`, { baseURL: API_V1_BASE })
    .then(() => '')
}

/** 单本书的分组归属：走书架 PATCH 的 group 字段（0 = 移出全部分组） */
export function saveBookGroupId(bookId: string, groupId: number) {
  return http
    .patch(`/me/shelf/${bookId}`, { group: groupId }, { baseURL: API_V1_BASE })
    .then(() => '')
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
/**
 * 给图片管道地址追加目标像素宽（`?w=`）：后端按宽度派生缓存（clamp 64..=2048、
 * 只降不升），配合 Accept 头的 webp 协商省流。只认 `/reader3/image/<id>` 与
 * `/reader3/cover?path=...`；本地 epub 资源、第三方直链与其他地址原样返回
 * （加了参数也没人认，反而污染缓存键之外的语义）。
 */
export function withImageWidth(url: string, width?: number) {
  if (!width || !Number.isFinite(width) || width <= 0) return url
  if (!url.startsWith('/reader3/image/') && !url.startsWith('/reader3/cover?')) return url
  const sep = url.includes('?') ? '&' : '?'
  return `${url}${sep}w=${Math.round(width)}`
}

export function getCoverUrl(coverUrl?: string, width?: number) {
  if (!coverUrl) return ''
  if (coverUrl.startsWith('/reader3/localEpubAsset')) {
    return withAppBase(coverUrl)
  }
  // 后端已经把书源地址换成了本站取图地址（`/reader3/image/<id>`）：
  // id 由后端映射表维护，前端不接触会过期的书源地址，会话 Cookie 随 <img> 自动携带
  if (coverUrl.startsWith('/reader3/image/')) {
    return withAppBase(withImageWidth(coverUrl, width))
  }
  if (coverUrl.startsWith('http') || coverUrl.startsWith('/')) {
    // 兼容入口：customCoverUrl 是用户自设的任意地址，走封面接口代理
    const params = new URLSearchParams({ path: coverUrl })
    return withAppBase(withImageWidth(`/reader3/cover?${params.toString()}`, width))
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
