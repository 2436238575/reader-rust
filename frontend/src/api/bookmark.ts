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

/** 批量保存的分片大小：服务端上限 5000 条，但 /v1 默认请求体 1MB 装不下
 *  真实划线（bookText + content 约 1.5KB/条），按 500 分片串行（约 750KB/片） */
const MARKS_SAVE_CHUNK = 500

/**
 * 批量保存（WebDAV 恢复）：指向未登记书籍的条目被服务端跳过；按 500 分片
 * 串行发送，整本恢复不会撞请求体上限
 */
export async function saveBookmarks(bookmarks: Bookmark[]) {
  for (let i = 0; i < bookmarks.length; i += MARKS_SAVE_CHUNK) {
    await http.post('/me/marks:batch', bookmarks.slice(i, i + MARKS_SAVE_CHUNK), {
      baseURL: API_V1_BASE,
    })
  }
  return ''
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
