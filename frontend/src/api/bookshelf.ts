import http from './http'
import type { Book, BookChapter, BookGroup, ChapterImages } from '../types'
import { appendAuthQueryParams } from '../utils/secureAccess'
import { withAppBase } from '../utils/appBase'

export function getBookshelf() {
  return http.get<Book[]>('/getBookshelf').then((r) => r.data)
}

export function getBookshelfWithCacheInfo() {
  return http.get<Book[]>('/getShelfBookWithCacheInfo').then((r) => r.data)
}

export function getShelfBook(bookId: string) {
  return http.post<Book>('/getShelfBook', { bookId }).then((r) => r.data)
}

export function saveBook(book: Partial<Book>) {
  return http.post<Book>('/saveBook', book).then((r) => r.data)
}

export function saveBooks(books: Partial<Book>[]) {
  return http.post<Book[]>('/saveBooks', books).then((r) => r.data)
}

/** WebDAV 恢复：整架覆盖 + 登记表清表重建（备份自带的 bookId 原位恢复） */
export function restoreBooks(books: Partial<Book>[]) {
  return http.post<Book[]>('/saveBooks', { books, restore: true }).then((r) => r.data)
}

export function uploadTxtBook(file: File) {
  const formData = new FormData()
  formData.append('file', file)
  return http
    .post<Book>('/uploadTxtBook', formData, {
      headers: { 'Content-Type': 'multipart/form-data' },
    })
    .then((r) => r.data)
}

export function uploadEpubBook(file: File) {
  const formData = new FormData()
  formData.append('file', file)
  return http
    .post<Book>('/uploadEpubBook', formData, {
      headers: { 'Content-Type': 'multipart/form-data' },
    })
    .then((r) => r.data)
}

export function deleteBook(bookId: string) {
  return http.post<string>('/deleteBook', { bookId }).then((r) => r.data)
}

export function deleteBooks(bookIds: string[]) {
  return http.post<{ deleted: number }>('/deleteBooks', { bookIds }).then((r) => r.data)
}

export function getBookInfo(bookId: string) {
  return http.post<Book>('/getBookInfo', { bookId }).then((r) => r.data)
}

export function getChapterList(params: { bookId: string; refresh?: number }) {
  return http.post<BookChapter[]>('/getChapterList', params).then((r) => r.data)
}

export function getBookContent(params: {
  bookId: string
  chapterUrl: string
  index?: number
  refresh?: number
  /** `upstream`：只读上游基准（缓存/本地原文），不读编辑版、不抓上游（diff 通道） */
  variant?: 'upstream'
}) {
  return http.post<string>('/getBookContent', params).then((r) => r.data)
}

/** 本章配图；书源没声明配图规则时返回 `enabled: false` */
export function getChapterImages(params: { bookId: string; chapterUrl: string }) {
  return http.post<ChapterImages>('/getChapterImages', params).then((r) => r.data)
}

export function saveBookProgress(params: { bookId: string; index: number; position?: number }) {
  return http.post<string>('/saveBookProgress', params).then((r) => r.data)
}

export function deleteBookCache(bookId: string) {
  return http.post('/deleteBookCache', { bookId }).then((r) => r.data)
}

// ─── Groups ───
export function getBookGroups() {
  return http.get<BookGroup[]>('/getBookGroups').then((r) => r.data)
}

export function saveBookGroup(group: BookGroup) {
  return http.post<string>('/saveBookGroup', group).then((r) => r.data)
}

export function deleteBookGroup(groupId: number) {
  return http.post<string>('/deleteBookGroup', { groupId }).then((r) => r.data)
}

export function saveBookGroupId(bookId: string, groupId: number) {
  return http.post<string>('/saveBookGroupId', { bookId, groupId }).then((r) => r.data)
}

/** 换源：目标是换源候选（搜索结果）自己的 bookId，当前书的 bookId 不变 */
export function setBookSource(params: { bookId: string; targetBookId: string }) {
  return http.post<Book>('/setBookSource', params).then((r) => r.data)
}

// ─── Cover helper ───
export function getCoverUrl(coverUrl?: string) {
  if (!coverUrl) return ''
  if (coverUrl.startsWith('/reader3/localEpubAsset')) {
    return withAuthQuery(coverUrl)
  }
  // 后端已经把书源地址换成了本站取图地址（`/reader3/image/<id>`）：
  // id 由后端映射表维护，前端不接触会过期的书源地址，补个令牌直接用
  if (coverUrl.startsWith('/reader3/image/')) {
    return withAuthQuery(coverUrl)
  }
  if (coverUrl.startsWith('http') || coverUrl.startsWith('/')) {
    // 兼容入口：customCoverUrl 是用户自设的任意地址，走封面接口代理
    const params = new URLSearchParams({ path: coverUrl })
    appendAuthQueryParams(params)
    return withAppBase(`/reader3/cover?${params.toString()}`)
  }
  return coverUrl
}

/// 给需要鉴权的直链（本地书资源、封面、配图等）补上令牌与部署前缀。
/// 令牌只发给本站路径：完整的 http(s) 地址指向第三方主机，拼上 accessToken
/// 等于把登录态随浏览器取图送出去——外源 URL 原样返回（约定上后端已把书源
/// 图片收口为 /reader3/image/<id>，走到这里的外源地址是兜底场景，宁可不渲染）。
export function withAuthQuery(url: string) {
  if (/^https?:\/\//i.test(url)) return url
  const [path, rawQuery = ''] = url.split('?')
  const params = new URLSearchParams(rawQuery)
  appendAuthQueryParams(params)
  return `${withAppBase(path)}?${params.toString()}`
}
