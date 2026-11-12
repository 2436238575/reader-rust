import http from './http'
import { API_V1_BASE } from '../utils/appBase'
import type { SearchBook } from '../types'

export interface ExploreBookParams {
  bookSourceId: number
  ruleFindUrl: string
  page?: number
}

/**
 * 获取探索发现的书本列表（v1）。
 *
 * 身份是 `src_<n>`；封面已由后端收口为图片管道地址。书单产出即登记，
 * 每项带 `bookId`。
 */
export function exploreBook(params: ExploreBookParams) {
  const sourceId = `src_${params.bookSourceId}`
  return http
    .get<SearchBook[]>(`/sources/${sourceId}/explore`, {
      baseURL: API_V1_BASE,
      params: {
        url: params.ruleFindUrl,
        ...(params.page != null ? { page: params.page } : {}),
      },
    })
    .then((r) => r.data)
}
