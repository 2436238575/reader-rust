import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import { exploreBook } from './explore'
import { API_V1_BASE } from '../utils/appBase'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
  },
}))

const getMock = http.get as unknown as Mock

describe('exploreBook（v1 接线）', () => {
  beforeEach(() => {
    getMock.mockReset().mockResolvedValue({ data: [] })
  })

  it('GET 拼 src_<n> 路径并透传 url/page', async () => {
    getMock.mockResolvedValue({
      data: [{ name: '书', bookId: 'book_1', bookUrl: 'http://up.example/book/1' }],
    })

    const result = await exploreBook({
      bookSourceId: 7,
      ruleFindUrl: 'http://up.example/discover',
      page: 2,
    })

    expect(getMock).toHaveBeenCalledTimes(1)
    const [url, config] = getMock.mock.calls[0]
    expect(url).toBe('/sources/src_7/explore')
    expect(config.baseURL).toBe(API_V1_BASE)
    expect(config.params).toEqual({
      url: 'http://up.example/discover',
      page: 2,
    })
    expect(result[0].name).toBe('书')
  })

  it('page 缺省时不传 page 参数', async () => {
    await exploreBook({ bookSourceId: 1, ruleFindUrl: 'http://up.example/x' })
    const [, config] = getMock.mock.calls[0]
    expect(config.params).toEqual({ url: 'http://up.example/x' })
  })
})
