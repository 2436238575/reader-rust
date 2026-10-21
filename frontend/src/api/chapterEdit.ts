import http from './http'

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
