import http, { UPLOAD_TIMEOUT_MS } from './http'
import { API_V1_BASE } from '../utils/appBase'
import type { ChapterVersionKind } from './bookshelf'
import type { ParagraphDiffOp } from '../utils/chapterDiff'

/**
 * 章节正文的用户版本、版本时间线与版本选择（v1）。
 *
 * 身份是 `(bookId, index)`：index 经章节登记表解析出内部 chapterUrl，
 * 客户端不再携带上游地址（红线 6）。版本号在线缆上是 `cv_<n>` 字符串，
 * 适配层转回数字（调用方仍用数字比较）。
 */

function cvId(versionId: number): string {
  return `cv_${versionId}`
}

function parseVersionId(id: string | null | undefined): number | undefined {
  if (!id) return undefined
  const parsed = Number(id.replace(/^cv_/, ''))
  return Number.isFinite(parsed) ? parsed : undefined
}

export interface SaveChapterEditParams {
  bookId: string
  /** 章节序号（登记表分配的稳定序号） */
  index: number
  content: string
  /** 编辑时看到的上游版本；不传则后端取当前最新上游 */
  baseVersionId?: number
  note?: string
}

export interface ImportChapterVersionParams {
  bookId: string
  index: number
  content: string
  baseVersionId?: number
  note?: string
  /** 默认只进时间线，不改变当前显示 */
  makeCurrent?: boolean
  /** 手动补历史时可指定时间（毫秒） */
  createdAt?: number
}

export interface ChapterTimelineItem {
  versionId: number
  kind: ChapterVersionKind
  createdAt: number
  rawSize: number
  /** 正文字数（字符数） */
  chars: number
  chapterIndex: number
  chapterTitle: string
  note: string
  bookSourceId?: number
  bookSourceName: string
  baseVersionId?: number
  isCurrent: boolean
  isLatestUpstream: boolean
  isLatestUser: boolean
}

export interface ChapterTimeline {
  items: ChapterTimelineItem[]
  latestUpstreamVersionId?: number
  latestUserVersionId?: number
  selectedVersionId?: number
  effectiveVersionId?: number
}

export interface ChapterVersionDetail {
  versionId: number
  kind: ChapterVersionKind
  baseVersionId?: number
  createdAt: number
  rawSize: number
  chars: number
  note: string
  bookSourceName: string
  chapterIndex: number
  chapterTitle: string
  content: string
}

/** 保存编辑（生成新用户版本，设为当前显示） */
export function saveChapterEdit(params: SaveChapterEditParams) {
  return http
    .put<{ edited: boolean; versionId: string }>(
      `/books/${params.bookId}/chapters/${params.index}/content`,
      {
        content: params.content,
        ...(params.baseVersionId != null
          ? { baseVersionId: cvId(params.baseVersionId) }
          : {}),
        ...(params.note ? { note: params.note } : {}),
      },
      { baseURL: API_V1_BASE },
    )
    .then((r) => ({ edited: r.data.edited, versionId: parseVersionId(r.data.versionId) ?? 0 }))
}

/** 当前用户编辑版（编辑器预填用）；没有编辑版本时返回 null */
export function getChapterEdit(bookId: string, index: number) {
  return http
    .get<{ content: string; versionId: string; baseVersionId?: string; kind: string } | null>(
      `/books/${bookId}/chapters/${index}/content/edit`,
      { baseURL: API_V1_BASE },
    )
    .then((r) => {
      if (r.data == null || r.data.content == null) return null
      return {
        content: r.data.content,
        versionId: parseVersionId(r.data.versionId) ?? 0,
        baseVersionId: parseVersionId(r.data.baseVersionId),
        kind: r.data.kind,
      }
    })
    .catch(() => null)
}

/** 有用户版本的章节地址列表（过渡期诊断） */
export function listChapterEdits(bookId: string) {
  return http
    .get<{ marks: Record<string, unknown> }>(`/books/${bookId}/chapter-marks`, {
      baseURL: API_V1_BASE,
    })
    .then((r) => ({ urls: Object.keys(r.data?.marks ?? {}) }))
}

/** 一章当前显示版本的标记（目录版本标签用） */
export interface ChapterVersionMark {
  kind: ChapterVersionKind
  note: string
  createdAt: number
  isLatestUpstream: boolean
}

/**
 * 每章当前显示版本的标记：chapterUrl → mark。只含有版本记录的章；
 * 没有的章调用方按「最新上游」处理。键沿用 chapterUrl（过渡期例外，
 * 与 v1 章节列表的只读 url 字段一致）。
 */
export function getChapterVersionMarks(bookId: string) {
  return http
    .get<{ marks: Record<string, ChapterVersionMark> }>(`/books/${bookId}/chapter-marks`, {
      baseURL: API_V1_BASE,
    })
    .then((r) => r.data?.marks ?? {})
}

/** 取消当前编辑（清用户版本指针，历史保留），显示回落到最新上游 */
export function deleteChapterEdit(bookId: string, index: number) {
  return http
    .delete(`/books/${bookId}/chapters/${index}/content`, { baseURL: API_V1_BASE })
    .then(() => ({ edited: false }))
}

/** 版本时间线（倒序，元数据不含正文） */
export function getChapterTimeline(params: {
  bookId: string
  index: number
  limit?: number
  beforeVersionId?: number
}) {
  return http
    .get<ChapterTimelineWire>(`/books/${params.bookId}/chapters/${params.index}/versions`, {
      baseURL: API_V1_BASE,
      params: {
        ...(params.limit != null ? { limit: params.limit } : {}),
        ...(params.beforeVersionId != null
          ? { beforeVersionId: params.beforeVersionId }
          : {}),
      },
    })
    .then((r) => mapTimeline(r.data))
}

/** 单个版本的正文（预览用，不改变当前选择） */
export function getChapterVersion(bookId: string, index: number, versionId: number) {
  return http
    .get<ChapterVersionDetailWire>(
      `/books/${bookId}/chapters/${index}/versions/${cvId(versionId)}`,
      { baseURL: API_V1_BASE },
    )
    .then((r) => mapVersionDetail(r.data))
}

/** 任意两个版本之间的 diff */
export function getChapterVersionDiff(params: {
  bookId: string
  index: number
  fromVersionId: number
  toVersionId: number
}) {
  return http
    .get<{ fromVersionId: string; toVersionId: string; ops: ParagraphDiffOp[] }>(
      `/books/${params.bookId}/chapters/${params.index}/versions/${cvId(params.fromVersionId)}/diff`,
      {
        baseURL: API_V1_BASE,
        params: { against: cvId(params.toVersionId) },
      },
    )
    .then((r) => ({
      fromVersionId: parseVersionId(r.data.fromVersionId) ?? 0,
      toVersionId: parseVersionId(r.data.toVersionId) ?? 0,
      ops: r.data.ops,
    }))
}

/** `versionId` 为空表示跟随最新 */
export function selectChapterVersion(
  bookId: string,
  index: number,
  versionId: number | null,
) {
  return http
    .post<{ selectedVersionId?: string; effectiveVersionId?: string }>(
      `/books/${bookId}/chapters/${index}/content:select`,
      { versionId: versionId != null ? cvId(versionId) : null },
      { baseURL: API_V1_BASE },
    )
    .then((r) => ({
      selectedVersionId: parseVersionId(r.data.selectedVersionId),
      effectiveVersionId: parseVersionId(r.data.effectiveVersionId),
    }))
}

/**
 * 整书换回最新上游：清掉本书全部版本选择与用户版本指针，每章显示
 * 回落到最新上游（本地书回到原文）。历史版本保留。
 */
export function resetBookToUpstream(bookId: string) {
  return http
    .post<{ cleared: number }>(
      `/books/${bookId}/content:reset`,
      {},
      { baseURL: API_V1_BASE },
    )
    .then((r) => r.data)
}

/** 手动导入一个历史版本。默认只进时间线，不改变当前显示 */
export function importChapterVersion(params: ImportChapterVersionParams) {
  return http
    .post<{ versionId: string; becameCurrent: boolean }>(
      `/books/${params.bookId}/chapters/${params.index}/content:import`,
      {
        content: params.content,
        ...(params.baseVersionId != null
          ? { baseVersionId: cvId(params.baseVersionId) }
          : {}),
        ...(params.note ? { note: params.note } : {}),
        ...(params.makeCurrent != null ? { makeCurrent: params.makeCurrent } : {}),
        ...(params.createdAt != null ? { createdAt: params.createdAt } : {}),
      },
      { baseURL: API_V1_BASE },
    )
    .then((r) => ({
      versionId: parseVersionId(r.data.versionId) ?? 0,
      becameCurrent: r.data.becameCurrent,
    }))
}

export interface RenameChapterResult {
  /** false 表示本次实际是「恢复原名」（title 为空或与目录原标题相同） */
  renamed: boolean
  /** 当前生效标题 */
  title: string
  /** 目录原标题（目录未缓存时为空串） */
  originalTitle: string
  hasOverride: boolean
}

/**
 * 改当前章节名：走持久覆盖层。`title` 传 null/空串 = 恢复原名。
 * 不写正文版本，时间线保留各版本抓取时的标题快照。
 */
export function renameChapter(params: {
  bookId: string
  index: number
  title: string | null
}) {
  const rename = (title: string) =>
    http
      .patch<rename_wire.RenamePayload>(
        `/books/${params.bookId}/chapters/${params.index}`,
        { title },
        { baseURL: API_V1_BASE },
      )
      .then((r) => r.data)
  const restore = () =>
    http
      .delete<rename_wire.RenamePayload>(`/books/${params.bookId}/chapters/${params.index}/title`, {
        baseURL: API_V1_BASE,
      })
      .then((r) => r.data)

  const apply = (data: rename_wire.RenamePayload): RenameChapterResult => ({
    renamed: data.renamed,
    title: data.title,
    originalTitle: data.originalTitle,
    hasOverride: data.hasOverride,
  })

  if (params.title) {
    return rename(params.title).then(apply)
  }
  return restore().then(apply)
}

/** rename 域的线缆形状（与 v1 handler 一致） */
namespace rename_wire {
  export interface RenamePayload {
    renamed: boolean
    title: string
    originalTitle: string
    hasOverride: boolean
  }
}

/** 当前章的标题覆盖状态（改名输入框初始态） */
export function getChapterTitleOverride(bookId: string, index: number) {
  return http
    .get<rename_wire.RenamePayload>(`/books/${bookId}/chapters/${index}/title`, {
      baseURL: API_V1_BASE,
    })
    .then((r) => ({
      title: r.data.title,
      originalTitle: r.data.originalTitle,
      hasOverride: r.data.hasOverride,
    }))
}

// ─── 整本 TXT 导入链 ─────────────────────────────────────────────────

export type ChapterImportMode = 'auto' | 'single' | 'book'

export interface ChapterImportPreviewChapter {
  sourceIndex: number
  title: string
  chars: number
  excerpt: string
  status: 'matched' | 'empty' | 'unmatched'
  matchKind: 'title' | 'number' | 'order' | 'single' | 'none'
  chapterIndex: number | null
  chapterUrl: string | null
  chapterTitle: string | null
  /** 目录当前生效标题（含已有覆盖）；matched 章才有意义 */
  currentTitle: string | null
  /** TXT 解析标题与生效标题是否不同（导入时将改名的章） */
  titleChanged: boolean
  /** 是否有当前缓存正文可用来对比 */
  cached: boolean
  /** 是否与当前缓存正文不同；没有缓存时为 false */
  changed: boolean
}

export interface ChapterImportPreview {
  mode: ChapterImportMode
  total: number
  matched: number
  unmatched: number
  empty: number
  changed: number
  cached: number
  uncached: number
  /** 详情对比与导入复用的短期令牌；正文过大时为空（此时导入需重新上传正文） */
  previewId: string | null
  chapters: ChapterImportPreviewChapter[]
}

export interface ChapterImportDiff {
  changed: boolean | null
  ops: ParagraphDiffOp[] | null
  diffError: string
  currentExcerpt: string
  currentChars: number | null
  importChars: number
}

export interface ChapterImportResult {
  mode: ChapterImportMode
  selected: number
  imported: number
  unchanged: number
  skipped: number
  /** 应用到标题覆盖层的章节数（applyTitles 开启且标题有变化时写入） */
  titled: number
  failed: number
  errors: string[]
}

export interface ChapterImportParams {
  bookId: string
  mode: ChapterImportMode
  chapterUrl?: string
  chapterIndex?: number
  chapterTitle?: string
  kind?: 'edited' | 'imported'
  note?: string
  makeCurrent?: boolean
  createdAt?: number
  /** 上传文件时优先使用文件，后端负责识别 UTF-8 / GB18030 / UTF-16。 */
  file?: File | null
  fileSize?: number
  text?: string
  uploadId?: string | null
  previewId?: string | null
  applyTitles?: boolean
}

function buildChapterImportForm(
  params: ChapterImportParams,
  selectedIndexes?: number[],
): FormData {
  const form = new FormData()
  form.append('bookId', params.bookId)
  form.append('mode', params.mode)
  if (params.chapterUrl) form.append('chapterUrl', params.chapterUrl)
  if (params.chapterIndex != null) form.append('chapterIndex', String(params.chapterIndex))
  if (params.chapterTitle) form.append('chapterTitle', params.chapterTitle)
  if (params.kind) form.append('kind', params.kind)
  if (params.note) form.append('note', params.note)
  if (params.makeCurrent != null) form.append('makeCurrent', String(params.makeCurrent))
  if (params.createdAt != null) form.append('createdAt', String(params.createdAt))
  if (params.applyTitles != null) form.append('applyTitles', String(params.applyTitles))
  if (params.previewId) {
    form.append('previewId', params.previewId)
  } else if (params.uploadId) {
    form.append('uploadId', params.uploadId)
  } else if (params.file) {
    form.append('fileSize', String(params.fileSize ?? params.file.size))
    form.append('file', params.file, params.file.name)
  } else if (params.text != null) {
    form.append('text', params.text)
  }
  if (selectedIndexes) form.append('selectedIndexes', selectedIndexes.join(','))
  return form
}

export interface ChunkUploadResult {
  uploadId: string
  /** 服务端当前已收到的字节数（重发同一片时可能大于本次分片末尾）。 */
  received: number
  complete: boolean
}

const CHUNK_INITIAL_BYTES = 512 * 1024
const CHUNK_MIN_BYTES = 128 * 1024
const CHUNK_MAX_BYTES = 8 * 1024 * 1024
const CHUNK_RETRY_LIMIT = 4

/** 超过该大小走自适应分片；小文件仍一次直传。 */
export const CHAPTER_IMPORT_CHUNK_THRESHOLD = CHUNK_INITIAL_BYTES

function uploadChapterImportChunk(params: {
  bookId: string
  uploadId: string | null
  offset: number
  fileSize: number
  chunk: Blob
}): Promise<ChunkUploadResult> {
  const form = new FormData()
  if (params.uploadId) form.append('uploadId', params.uploadId)
  form.append('offset', String(params.offset))
  form.append('fileSize', String(params.fileSize))
  form.append('chunk', params.chunk, 'chunk.bin')
  return http
    .post<ChunkUploadResult>(
      `/books/${params.bookId}/chapters:upload-import-chunk`,
      form,
      {
        headers: { 'Content-Type': 'multipart/form-data' },
        timeout: UPLOAD_TIMEOUT_MS,
        baseURL: API_V1_BASE,
      },
    )
    .then((r) => r.data)
}

/**
 * 自适应分片上传整本 TXT，返回预览/导入用的 `uploadId`。
 *
 * 反代/WAF 的单请求体积上限各不相同，这里不写死分片大小：
 * 从 512KB 起步，成功就温和放大，遇到 413/断连/5xx 就减半并把上限钉住。
 */
export async function uploadChapterImportFile(
  bookId: string,
  file: File,
  onProgress?: (uploaded: number, total: number) => void,
): Promise<string> {
  let uploadId: string | null = null
  let offset = 0
  let chunkSize = CHUNK_INITIAL_BYTES
  let ceiling = CHUNK_MAX_BYTES

  while (offset < file.size) {
    let attempt = 0
    for (;;) {
      const end = Math.min(offset + chunkSize, file.size)
      const chunk = file.slice(offset, end)
      try {
        const result = await uploadChapterImportChunk({
          bookId,
          uploadId,
          offset,
          fileSize: file.size,
          chunk,
        })
        uploadId = result.uploadId
        if (result.received <= offset) {
          throw new Error('上传进度异常，请重试')
        }
        offset = result.received
        chunkSize = Math.min(
          ceiling,
          Math.max(CHUNK_MIN_BYTES, Math.floor(chunkSize * 1.5)),
        )
        onProgress?.(offset, file.size)
        break
      } catch (error) {
        attempt += 1
        const status = (error as { status?: number }).status
        const retriable =
          status == null || status === 413 || status === 408 || status >= 500
        if (!retriable || attempt >= CHUNK_RETRY_LIMIT || chunkSize <= CHUNK_MIN_BYTES) {
          throw error
        }
        chunkSize = Math.max(CHUNK_MIN_BYTES, Math.floor(chunkSize / 2))
        ceiling = chunkSize
        await new Promise((resolve) => setTimeout(resolve, 200 * attempt))
      }
    }
  }
  if (!uploadId) throw new Error('上传失败')
  return uploadId
}

/** 预览单章 / 整本 TXT 的解析与匹配结果，不写库 */
export function previewChapterImport(params: ChapterImportParams) {
  return http
    .post<ChapterImportPreview>(
      `/books/${params.bookId}/chapters:preview-import`,
      buildChapterImportForm(params),
      {
        headers: { 'Content-Type': 'multipart/form-data' },
        timeout: UPLOAD_TIMEOUT_MS,
        baseURL: API_V1_BASE,
      },
    )
    .then((r) => r.data)
}

/** 按预览结果批量导入。`selectedIndexes` 传入空数组表示一章都不导入 */
export function importChapterText(params: ChapterImportParams, selectedIndexes?: number[]) {
  return http
    .post<ChapterImportResult>(
      `/books/${params.bookId}/chapters:import`,
      buildChapterImportForm(params, selectedIndexes),
      {
        headers: { 'Content-Type': 'multipart/form-data' },
        timeout: UPLOAD_TIMEOUT_MS,
        baseURL: API_V1_BASE,
      },
    )
    .then((r) => r.data)
}

/** 预览里点开某章时按需取它与当前正文的差异 */
export function getChapterImportDiff(previewId: string, bookId: string, sourceIndex: number) {
  return http
    .get<ChapterImportDiff>(`/books/${bookId}/chapters/import-diff`, {
      baseURL: API_V1_BASE,
      params: { previewId, sourceIndex },
    })
    .then((r) => r.data)
}

// ── typed 线缆形状映射 ───────────────────────────────────────────────

interface ChapterTimelineWire {
  items: {
    versionId: string
    kind: string
    createdAt: number
    rawSize: number
    chars: number
    chapterIndex: number
    chapterTitle: string
    note: string
    bookSourceId?: number
    bookSourceName: string
    baseVersionId?: string
    isCurrent: boolean
    isLatestUpstream: boolean
    isLatestUser: boolean
  }[]
  latestUpstreamVersionId?: string
  latestUserVersionId?: string
  selectedVersionId?: string
  effectiveVersionId?: string
}

function mapTimeline(wire: ChapterTimelineWire): ChapterTimeline {
  return {
    items: wire.items.map((item) => ({
      ...item,
      versionId: parseVersionId(item.versionId) ?? 0,
      kind: item.kind as ChapterVersionKind,
      baseVersionId: parseVersionId(item.baseVersionId),
    })),
    latestUpstreamVersionId: parseVersionId(wire.latestUpstreamVersionId),
    latestUserVersionId: parseVersionId(wire.latestUserVersionId),
    selectedVersionId: parseVersionId(wire.selectedVersionId),
    effectiveVersionId: parseVersionId(wire.effectiveVersionId),
  }
}

interface ChapterVersionDetailWire {
  versionId: string
  kind: string
  baseVersionId?: string
  createdAt: number
  rawSize: number
  chars: number
  note: string
  bookSourceName: string
  chapterIndex: number
  chapterTitle: string
  content: string
}

function mapVersionDetail(wire: ChapterVersionDetailWire): ChapterVersionDetail {
  return {
    ...wire,
    versionId: parseVersionId(wire.versionId) ?? 0,
    kind: wire.kind as ChapterVersionKind,
    baseVersionId: parseVersionId(wire.baseVersionId),
  }
}
