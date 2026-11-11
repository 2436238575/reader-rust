import http from './http'
import { API_V1_BASE } from '../utils/appBase'
import type { Bookmark } from '../types'

/**
 * 获取所有书签与划线（`?bookId=` 可过滤单本）
 */
export function getBookmarks() {
  return http
    .get<Bookmark[]>('/me/marks', { baseURL: API_V1_BASE })
    .then((r) => r.data)
}

/**
 * 保存单个书签/划线（同身份覆盖）
 */
export function saveBookmark(bookmark: Bookmark) {
  return http
    .post('/me/marks', bookmark, { baseURL: API_V1_BASE })
    .then(() => '')
}

/**
 * 批量保存（WebDAV 恢复）：指向未登记书籍的条目被服务端跳过
 */
export function saveBookmarks(bookmarks: Bookmark[]) {
  return http
    .post('/me/marks:batch', bookmarks, { baseURL: API_V1_BASE })
    .then(() => '')
}

/**
 * 删除单个书签/划线（按身份五元组，走批量端点单条复用）
 */
export function deleteBookmark(bookmark: Bookmark) {
  return http
    .post('/me/marks:batchDelete', [bookmark], { baseURL: API_V1_BASE })
    .then(() => '')
}

/**
 * 批量删除书签/划线
 */
export function deleteBookmarks(bookmarks: Bookmark[]) {
  return http
    .post('/me/marks:batchDelete', bookmarks, { baseURL: API_V1_BASE })
    .then(() => '')
}
