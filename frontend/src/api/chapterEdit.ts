import http from './http'
import type { ParagraphDiffOp } from '../utils/chapterDiff'

/**
 * 章节正文编辑版（后端存在 data/ 用户资产区，与缓存分离）。
 * 阅读链路里编辑版优先于上游；`variant: 'upstream'` 拿 diff 基准。
 */

export interface SaveChapterEditParams {
  bookId: string
  chapterUrl: string
  chapterIndex: number
  chapterTitle: string
  content: string
}

export function saveChapterEdit(params: SaveChapterEditParams) {
  return http.post<{ edited: boolean }>('/saveChapterEdit', params).then((r) => r.data)
}

export function getChapterEdit(bookId: string, chapterUrl: string) {
  return http
    .get<{ content: string } | null>('/getChapterEdit', { params: { bookId, chapterUrl } })
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

/**
 * 用后端系统 git 算段落 diff。`fallback: true` 表示服务器没装 git
 * （或执行失败），调用方回退到前端本地计算。输入是切好的段落数组。
 */
export function diffChapterEdit(base: string[], edited: string[]) {
  return http
    .post<{ fallback: boolean; ops?: ParagraphDiffOp[] }>('/diffChapterEdit', { base, edited })
    .then((r) => r.data)
}
