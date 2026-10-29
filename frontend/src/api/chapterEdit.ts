import http, { UPLOAD_TIMEOUT_MS } from './http'
import type { ChapterVersionKind } from './bookshelf'
import type { ParagraphDiffOp } from '../utils/chapterDiff'

/**
 * 章节正文的用户版本、版本时间线与版本选择。
 *
 * 编辑版和手动导入都进正文池；删除当前用户版本只清指针，历史仍然保留。
 */

export interface SaveChapterEditParams {
  bookId: string
  chapterUrl: string
  chapterIndex: number
  chapterTitle: string
  content: string
  /** 编辑时看到的上游版本；不传则后端取当前最新上游 */
  baseVersionId?: number
  note?: string
}

export interface ImportChapterVersionParams {
  bookId: string
  chapterUrl: string
  content: string
  chapterIndex?: number
  chapterTitle?: string
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

export function saveChapterEdit(params: SaveChapterEditParams) {
  return http
    .post<{ edited: boolean; versionId: number }>('/saveChapterEdit', params)
    .then((r) => r.data)
}

export function getChapterEdit(bookId: string, chapterUrl: string) {
  return http
    .get<{ content: string; versionId: number; baseVersionId?: number; kind: string } | null>(
      '/getChapterEdit',
      { params: { bookId, chapterUrl } }
    )
    .then((r) => r.data)
}

export function listChapterEdits(bookId: string) {
  return http
    .get<{ urls: string[] }>('/listChapterEdits', { params: { bookId } })
    .then((r) => r.data?.urls ?? [])
}

/** 一章当前显示版本的标记（目录版本标签用）。 */
export interface ChapterVersionMark {
  kind: ChapterVersionKind
  note: string
  createdAt: number
  isLatestUpstream: boolean
}

/**
 * 每章当前显示版本的标记：chapterUrl → mark。只含有版本记录的章；
 * 没有的章调用方按「最新上游」处理。
 */
export function getChapterVersionMarks(bookId: string) {
  return http
    .get<{ marks: Record<string, ChapterVersionMark> }>('/chapterVersionMarks', {
      params: { bookId },
    })
    .then((r) => r.data?.marks ?? {})
}

export function deleteChapterEdit(bookId: string, chapterUrl: string) {
  return http
    .post<{ edited: boolean }>('/deleteChapterEdit', { bookId, chapterUrl })
    .then((r) => r.data)
}

export function getChapterTimeline(params: {
  bookId: string
  chapterUrl: string
  limit?: number
  beforeVersionId?: number
}) {
  return http.post<ChapterTimeline>('/getChapterTimeline', params).then((r) => r.data)
}

export function getChapterVersion(bookId: string, chapterUrl: string, versionId: number) {
  return http
    .post<ChapterVersionDetail>('/getChapterVersion', { bookId, chapterUrl, versionId })
    .then((r) => r.data)
}

export function getChapterVersionDiff(params: {
  bookId: string
  chapterUrl: string
  fromVersionId: number
  toVersionId: number
}) {
  return http
    .post<{ fromVersionId: number; toVersionId: number; ops: ParagraphDiffOp[] }>(
      '/getChapterVersionDiff',
      params
    )
    .then((r) => r.data)
}

/** `versionId` 为空表示跟随最新。 */
export function selectChapterVersion(bookId: string, chapterUrl: string, versionId: number | null) {
  return http
    .post<{ selectedVersionId?: number; effectiveVersionId?: number }>('/selectChapterVersion', {
      bookId,
      chapterUrl,
      versionId,
    })
    .then((r) => r.data)
}

/**
 * 整书换回最新上游：清掉本书全部版本选择与用户版本指针，每章显示
 * 回落到最新上游（本地书回到原文）。历史版本保留。`cleared` 是摘掉
 * 用户版本指针的章节数。
 */
export function resetBookToUpstream(bookId: string) {
  return http
    .post<{ cleared: number }>('/resetBookToUpstream', { bookId })
    .then((r) => r.data)
}

export function importChapterVersion(params: ImportChapterVersionParams) {
  return http
    .post<{ versionId: number; becameCurrent: boolean }>('/importChapterVersion', params)
    .then((r) => r.data)
}

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
  /**
   * 上传文件的原始字节数。后端用它核对实际收到的字节数——反向代理截断
   * multipart 后请求仍可能是合法的，只能用这个值发现「只收到半本书」。
   */
  fileSize?: number
  /** 没有文件时使用粘贴的文本。 */
  text?: string
  /**
   * 自适应分片上传返回的令牌：正文已经全部到达服务端，预览/导入不再上传
   * file/text。与 previewId 同时存在时优先用 previewId。
   */
  uploadId?: string | null
  /**
   * 导入时带上预览令牌（previewChapterImport 返回的 previewId）：后端直接复用
   * 预览暂存的切章/匹配结果，不再上传 file/text；预览过期或跨书会 400，需重新预览。
   */
  previewId?: string | null
}

function buildChapterImportForm(
  params: ChapterImportParams,
  selectedIndexes?: number[]
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
  if (params.previewId) {
    // 复用预览暂存：正文不再二次上传
    form.append('previewId', params.previewId)
  } else if (params.uploadId) {
    // 分片已经全部上传：预览/导入只带令牌
    form.append('uploadId', params.uploadId)
  } else if (params.file) {
    // fileSize 必须排在 file 前面：代理截断正文时，小字段先到，后端才有
    // 依据发现文件不完整。
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
    .post<ChunkUploadResult>('/uploadChapterImportChunk', form, {
      headers: { 'Content-Type': 'multipart/form-data' },
      timeout: UPLOAD_TIMEOUT_MS,
    })
    .then((r) => r.data)
}

/**
 * 自适应分片上传整本 TXT，返回预览/导入用的 `uploadId`。
 *
 * 反代/WAF 的单请求体积上限各不相同（nginx 默认 1m），这里不写死分片大小：
 * 从 512KB 起步，成功就温和放大，遇到 413/断连/5xx 就减半并把上限钉住，
 * 后续分片不再涨回去。这样既能绕开小的限制，又不会在宽松链路上拆得太碎。
 */
export async function uploadChapterImportFile(
  file: File,
  onProgress?: (uploaded: number, total: number) => void
): Promise<string> {
  let uploadId: string | null = null
  let offset = 0
  let chunkSize = CHUNK_INITIAL_BYTES
  // 失败过的档位是硬上限：成功后再放大也不能超过它
  let ceiling = CHUNK_MAX_BYTES

  while (offset < file.size) {
    let attempt = 0
    for (;;) {
      // 每次重试都按当前 chunkSize 重新切片：失败减半后必须真的变小
      const end = Math.min(offset + chunkSize, file.size)
      const chunk = file.slice(offset, end)
      try {
        const result = await uploadChapterImportChunk({
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
          Math.max(CHUNK_MIN_BYTES, Math.floor(chunkSize * 1.5))
        )
        onProgress?.(offset, file.size)
        break
      } catch (error) {
        attempt += 1
        const status = (error as { status?: number }).status
        // 413 是明确的体积上限；无状态码是断连/超时；5xx 也值得缩片重试。
        // 其他 4xx（uploadId 过期、参数错误）缩片没有意义，直接抛出。
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

export function previewChapterImport(params: ChapterImportParams) {
  return http
    .post<ChapterImportPreview>('/previewChapterImport', buildChapterImportForm(params), {
      headers: { 'Content-Type': 'multipart/form-data' },
      timeout: UPLOAD_TIMEOUT_MS,
    })
    .then((r) => r.data)
}

/** `selectedIndexes` 传入空数组表示一章都不导入；不传表示全部已匹配章节。 */
export function importChapterText(params: ChapterImportParams, selectedIndexes?: number[]) {
  return http
    .post<ChapterImportResult>('/importChapterText', buildChapterImportForm(params, selectedIndexes), {
      headers: { 'Content-Type': 'multipart/form-data' },
      timeout: UPLOAD_TIMEOUT_MS,
    })
    .then((r) => r.data)
}

/** 预览里点开某章时按需取它与当前正文的差异。 */
export function getChapterImportDiff(previewId: string, sourceIndex: number) {
  return http
    .post<ChapterImportDiff>('/getChapterImportDiff', { previewId, sourceIndex })
    .then((r) => r.data)
}
