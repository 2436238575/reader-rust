import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import {
  deleteBookmark,
  deleteBookmarks,
  getBookmarks,
  saveBookmark,
  saveBookmarks,
} from './bookmark'
import { API_V1_BASE } from '../utils/appBase'
import type { Bookmark } from '../types'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
    delete: vi.fn(),
  },
}))

const getMock = http.get as unknown as Mock
const postMock = http.post as unknown as Mock

const mark: Bookmark = {
  bookId: 'book_1',
  time: 1760000000,
  bookName: '书',
  bookAuthor: '作者',
  chapterIndex: 3,
  chapterPos: 0,
  chapterName: '第四章',
  bookText: '正文',
  content: '',
  kind: 'bookmark',
}

describe('bookmark api（v1 marks）', () => {
  beforeEach(() => {
    getMock.mockReset()
    postMock.mockReset()
  })

  it('getBookmarks 走 v1 marks（可带 bookId 过滤）', async () => {
    getMock.mockResolvedValue({ data: [mark] })
    await getBookmarks()
    expect(getMock).toHaveBeenCalledWith('/me/marks', { baseURL: API_V1_BASE })
  })

  it('保存与批量保存', async () => {
    postMock.mockResolvedValue({ data: undefined, status: 204 })
    await saveBookmark(mark)
    expect(postMock).toHaveBeenCalledWith('/me/marks', mark, { baseURL: API_V1_BASE })

    await saveBookmarks([mark])
    expect(postMock).toHaveBeenCalledWith('/me/marks:batch', [mark], { baseURL: API_V1_BASE })
  })

  it('saveBookmarks 超过 500 条时按分片串行发送，切片不重不漏', async () => {
    postMock.mockResolvedValue({ data: { saved: 500, skipped: 0 }, status: 200 })
    const bookmarks = Array.from({ length: 1200 }, (_, index) => ({
      ...mark,
      chapterPos: index,
    }))

    await saveBookmarks(bookmarks)

    expect(postMock).toHaveBeenCalledTimes(3)
    expect(postMock.mock.calls[0][1]).toHaveLength(500)
    expect(postMock.mock.calls[1][1]).toHaveLength(500)
    expect(postMock.mock.calls[2][1]).toHaveLength(200)
    // 三片拼接必须与输入完全一致：分片循环的边界/越界在这里暴露
    const sent = [
      ...postMock.mock.calls[0][1],
      ...postMock.mock.calls[1][1],
      ...postMock.mock.calls[2][1],
    ]
    expect(sent).toEqual(bookmarks)
    for (const call of postMock.mock.calls) {
      expect(call[0]).toBe('/me/marks:batch')
      expect(call[2]).toEqual({ baseURL: API_V1_BASE })
    }
  })

  it('saveBookmarks 恰好 500 条只发一片（边界）', async () => {
    postMock.mockResolvedValue({ data: undefined, status: 200 })
    const bookmarks = Array.from({ length: 500 }, (_, index) => ({
      ...mark,
      chapterPos: index,
    }))

    await saveBookmarks(bookmarks)

    expect(postMock).toHaveBeenCalledTimes(1)
    expect(postMock.mock.calls[0][1]).toHaveLength(500)
  })

  it('删除按身份五元组走批量端点（单条复用）', async () => {
    postMock.mockResolvedValue({ data: { deleted: 1 } })
    await deleteBookmark(mark)
    expect(postMock).toHaveBeenCalledWith('/me/marks:batchDelete', [mark], {
      baseURL: API_V1_BASE,
    })

    await deleteBookmarks([mark])
    expect(postMock).toHaveBeenCalledWith('/me/marks:batchDelete', [mark], {
      baseURL: API_V1_BASE,
    })
  })
})
