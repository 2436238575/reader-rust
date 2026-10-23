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
