import http from './http'
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
  /** 详情对比用的短期令牌；正文过大时为空 */
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
  /** 没有文件时使用粘贴的文本。 */
  text?: string
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
  if (params.file) {
    form.append('file', params.file, params.file.name)
  } else if (params.text != null) {
    form.append('text', params.text)
  }
  if (selectedIndexes) form.append('selectedIndexes', selectedIndexes.join(','))
  return form
}

export function previewChapterImport(params: ChapterImportParams) {
  return http
    .post<ChapterImportPreview>('/previewChapterImport', buildChapterImportForm(params), {
      headers: { 'Content-Type': 'multipart/form-data' },
    })
    .then((r) => r.data)
}

/** `selectedIndexes` 传入空数组表示一章都不导入；不传表示全部已匹配章节。 */
export function importChapterText(params: ChapterImportParams, selectedIndexes?: number[]) {
  return http
    .post<ChapterImportResult>('/importChapterText', buildChapterImportForm(params, selectedIndexes), {
      headers: { 'Content-Type': 'multipart/form-data' },
    })
    .then((r) => r.data)
}

/** 预览里点开某章时按需取它与当前正文的差异。 */
export function getChapterImportDiff(previewId: string, sourceIndex: number) {
  return http
    .post<ChapterImportDiff>('/getChapterImportDiff', { previewId, sourceIndex })
    .then((r) => r.data)
}
