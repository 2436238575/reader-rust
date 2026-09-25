import http from './http'
import type { Book, BookChapter, BookGroup, ChapterImages } from '../types'
import { appendAuthQueryParams } from '../utils/secureAccess'

export function getBookshelf() {
  return http.get<Book[]>('/getBookshelf').then((r) => r.data)
}

export function getBookshelfWithCacheInfo() {
  return http.get<Book[]>('/getShelfBookWithCacheInfo').then((r) => r.data)
}

export function getShelfBook(url: string) {
  return http.post<Book>('/getShelfBook', { url }).then((r) => r.data)
}

export function saveBook(book: Partial<Book>) {
  return http.post<Book>('/saveBook', book).then((r) => r.data)
}

export function saveBooks(books: Partial<Book>[]) {
  return http.post<Book[]>('/saveBooks', books).then((r) => r.data)
}

export function uploadTxtBook(file: File) {
  const formData = new FormData()
  formData.append('file', file)
  return http.post<Book>('/uploadTxtBook', formData, {
    headers: { 'Content-Type': 'multipart/form-data' },
  }).then((r) => r.data)
}

export function uploadEpubBook(file: File) {
  const formData = new FormData()
  formData.append('file', file)
  return http.post<Book>('/uploadEpubBook', formData, {
    headers: { 'Content-Type': 'multipart/form-data' },
  }).then((r) => r.data)
}

export function deleteBook(book: Partial<Book>) {
  return http.post<string>('/deleteBook', book).then((r) => r.data)
}

export function deleteBooks(books: Partial<Book>[]) {
  return http.post<{ deleted: number }>('/deleteBooks', books).then((r) => r.data)
}

export function getBookInfo(url: string, origin?: string) {
  return http.post<Book>('/getBookInfo', { url, bookSourceUrl: origin }).then((r) => r.data)
}

export function getChapterList(params: {
  bookUrl?: string
  tocUrl?: string
  bookSourceUrl?: string
  refresh?: number
}) {
  return http.post<BookChapter[]>('/getChapterList', params).then((r) => r.data)
}

export function getBookContent(params: {
  chapterUrl?: string
  bookSourceUrl?: string
  index?: number
  refresh?: number
}) {
  return http.post<string>('/getBookContent', params).then((r) => r.data)
}

/** 本章配图；书源没声明配图规则时返回 `enabled: false` */
export function getChapterImages(params: {
  bookUrl: string
  chapterUrl: string
  bookSourceUrl?: string
}) {
  return http.post<ChapterImages>('/getChapterImages', params).then((r) => r.data)
}

export function saveBookProgress(params: {
  bookUrl: string
  index: number
  position?: number
}) {
  return http.post<string>('/saveBookProgress', params).then((r) => r.data)
}

export function deleteBookCache(bookUrl: string) {
  return http.post('/deleteBookCache', { bookUrl }).then((r) => r.data)
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

export function saveBookGroupId(bookUrl: string, groupId: number) {
  return http.post<string>('/saveBookGroupId', { bookUrl, groupId }).then((r) => r.data)
}

export function setBookSource(params: {
  bookUrl: string
  newUrl: string
  bookSourceUrl: string
}) {
  return http.post<Book>('/setBookSource', params).then((r) => r.data)
}

// ─── Cover helper ───
export function getCoverUrl(coverUrl?: string) {
  if (!coverUrl) return ''
  if (coverUrl.startsWith('/reader3/localEpubAsset')) {
    return withAuthQuery(coverUrl)
  }
  if (coverUrl.startsWith('http') || coverUrl.startsWith('/')) {
    // 封面接口需要鉴权，而 <img> 带不了请求头，只能把令牌放进查询参数
    const params = new URLSearchParams({ path: coverUrl })
    appendAuthQueryParams(params)
    return `/reader3/cover?${params.toString()}`
  }
  return coverUrl
}

/// 给需要鉴权的直链（本地书资源、封面等）补上令牌。
export function withAuthQuery(url: string) {
  const [path, rawQuery = ''] = url.split('?')
  const params = new URLSearchParams(rawQuery)
  appendAuthQueryParams(params)
  return `${path}?${params.toString()}`
}
