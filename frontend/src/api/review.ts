import http from './http'
import { API_V1_BASE } from '../utils/appBase'
import type { ParaReviewIndex, ReviewPage, ReviewResponse } from '../types'

/**
 * 评论接口（v1）。
 *
 * 身份是 `(bookId, index)`：index 经章节登记表解析出内部 chapterUrl，
 * 客户端不再携带上游地址。书源没声明评论规则时 `enabled` 为 false，
 * 前端据此隐藏入口。
 */
export interface ReviewParams {
  bookId: string
  /** 章节序号（登记表分配的稳定序号） */
  index: number
  page?: number
  count?: number
  /** `hot`（默认，站点热度序）或 `time`（按时间倒序） */
  sort?: ReviewSort
  /** true 时忽略分页：后端扫描全部评论页，返回作者评论/赞过/回复过的整条评论 */
  authorOnly?: boolean
  /** true 表示忽略 7 天缓存重新抓取 */
  refresh?: boolean
  /**
   * 当前显示的正文版本。评论按版本分开缓存，书源评论 URL 里可用
   * `{{variant}}` 区分；大多数站点的章评/段评与版本无关。
   */
  variant?: 'edited' | 'upstream'
}

export type ReviewSort = 'hot' | 'time'

function commentsQuery(params: ReviewParams, para?: number) {
  const query: Record<string, unknown> = {}
  if (params.page !== undefined) query.page = params.page
  if (params.count !== undefined) query.count = params.count
  if (params.sort) query.sort = params.sort
  if (params.authorOnly) query.authorOnly = true
  if (params.refresh) query.refresh = true
  // 服务端默认 edited，只有真看上游版才需要带
  if (params.variant === 'upstream') query.variant = params.variant
  if (para !== undefined) query.para = para
  return query
}

function commentsPath(params: ReviewParams) {
  return `/books/${params.bookId}/chapters/${params.index}/comments`
}

export function getChapterComments(params: ReviewParams) {
  return http
    .get<ReviewResponse<ReviewPage>>(commentsPath(params), {
      baseURL: API_V1_BASE,
      params: commentsQuery(params),
    })
    .then((r) => r.data)
}

export function getParaCommentIndex(params: Omit<ReviewParams, 'page' | 'count'>) {
  return http
    .get<ReviewResponse<ParaReviewIndex>>(`${commentsPath(params)}/index`, {
      baseURL: API_V1_BASE,
      params: commentsQuery(params),
    })
    .then((r) => r.data)
}

export function getParaComments(params: ReviewParams & { paraIndex: number }) {
  return http
    .get<ReviewResponse<ReviewPage>>(commentsPath(params), {
      baseURL: API_V1_BASE,
      params: commentsQuery(params, params.paraIndex),
    })
    .then((r) => r.data)
}
