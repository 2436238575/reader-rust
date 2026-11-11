import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import {
  deleteAllBookSources,
  deleteBookSource,
  deleteBookSources,
  deleteInvalidBookSources,
  getBookSource,
  parseSourceId,
  saveBookSource,
  saveBookSources,
} from './source'
import { API_V1_BASE } from '../utils/appBase'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
    put: vi.fn(),
    delete: vi.fn(),
  },
}))

const getMock = http.get as unknown as Mock
const postMock = http.post as unknown as Mock
const deleteMock = http.delete as unknown as Mock

describe('source 写链路（v1）', () => {
  beforeEach(() => {
    getMock.mockReset()
    postMock.mockReset()
    deleteMock.mockReset()
  })

  it('parseSourceId 解析 src_ 前缀', () => {
    expect(parseSourceId('src_12')).toBe(12)
    expect(parseSourceId('src_0')).toBe(0)
  })

  it('getBookSource 走 v1 详情（源材料透传，无需映射）', async () => {
    const source = { bookSourceName: '甲', bookSourceUrl: 'https://a.example.com', bookSourceId: 12 }
    getMock.mockResolvedValue({ data: source })

    const result = await getBookSource(12)

    expect(getMock).toHaveBeenCalledWith('/sources/src_12', { baseURL: API_V1_BASE })
    expect(result).toEqual(source)
  })

  it('saveBookSource 上送源材料并把 id 映射回数字', async () => {
    postMock.mockResolvedValue({ data: { id: 'src_9' } })
    const source = { bookSourceName: '甲', bookSourceUrl: 'https://a.example.com' }

    const result = await saveBookSource(source as never)

    expect(postMock).toHaveBeenCalledWith('/sources', source, { baseURL: API_V1_BASE })
    expect(result).toEqual({ saved: true, bookSourceId: 9 })
  })

  it('saveBookSources 包成 {sources} 批量上送', async () => {
    postMock.mockResolvedValue({ data: { count: 2 } })
    const sources = [{ bookSourceUrl: 'https://a.example.com' }, { bookSourceUrl: 'https://b.example.com' }]

    const result = await saveBookSources(sources as never)

    expect(postMock).toHaveBeenCalledWith(
      '/sources:batchUpsert',
      { sources },
      { baseURL: API_V1_BASE }
    )
    expect(result).toEqual({ saved: true, count: 2 })
  })

  it('删除三件套走 v1', async () => {
    deleteMock.mockResolvedValue({ data: undefined, status: 204 })
    await deleteBookSource(5)
    expect(deleteMock).toHaveBeenCalledWith('/sources/src_5', { baseURL: API_V1_BASE })

    postMock.mockResolvedValue({ data: { deleted: 2 } })
    await deleteBookSources([{ bookSourceId: 1 }, { bookSourceId: 2 }])
    expect(postMock).toHaveBeenCalledWith(
      '/sources:batchDelete',
      { ids: ['src_1', 'src_2'] },
      { baseURL: API_V1_BASE }
    )

    deleteMock.mockResolvedValue({ data: { deleted: 7 } })
    await deleteAllBookSources()
    expect(deleteMock).toHaveBeenCalledWith('/sources', {
      baseURL: API_V1_BASE,
      data: { confirm: 'DELETE_ALL' },
    })

    postMock.mockResolvedValue({ data: { deleted: 1 } })
    const result = await deleteInvalidBookSources()
    expect(postMock).toHaveBeenCalledWith('/sources:deleteInvalid', {}, { baseURL: API_V1_BASE })
    expect(result).toEqual({ deleted: 1 })
  })
})
