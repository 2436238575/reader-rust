import http from './http'
import type { ParaReviewIndex, ReviewPage, ReviewResponse } from '../types'

/**
 * 评论接口。
 *
 * 后端会先按书源的评论规则解析出真实地址，前端只需要给出「哪本书的哪一章」。
 * 书源没声明评论规则时 `enabled` 为 false，前端据此隐藏入口。
 */
export interface ReviewParams {
  bookUrl: string
  chapterUrl: string
  bookSourceUrl?: string
  page?: number
  count?: number
  /** `hot`（默认，站点热度序）或 `time`（按时间倒序） */
  sort?: ReviewSort
  /** true 时忽略分页：后端扫描全部评论页，返回作者评论/赞过/回复过的整条评论 */
  authorOnly?: boolean
  /** 1 表示忽略 7 天缓存重新抓取 */
  refresh?: number
}

export type ReviewSort = 'hot' | 'time'

export function getChapterComments(params: ReviewParams) {
  return http.post<ReviewResponse<ReviewPage>>('/getChapterComments', params).then((r) => r.data)
}

export function getParaCommentIndex(params: Omit<ReviewParams, 'page' | 'count'>) {
  return http
    .post<ReviewResponse<ParaReviewIndex>>('/getParaCommentIndex', params)
    .then((r) => r.data)
}

export function getParaComments(params: ReviewParams & { paraIndex: number }) {
  return http.post<ReviewResponse<ReviewPage>>('/getParaComments', params).then((r) => r.data)
}
