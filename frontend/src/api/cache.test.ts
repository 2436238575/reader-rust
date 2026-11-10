import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import { getCacheStats, purgeAllUserCache, purgeCacheByKind } from './cache'
import { API_V1_BASE } from '../utils/appBase'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
    delete: vi.fn(),
  },
}))

const getMock = http.get as unknown as Mock
const deleteMock = http.delete as unknown as Mock

describe('cache api（v1）', () => {
  beforeEach(() => {
    getMock.mockReset()
    deleteMock.mockReset()
  })

  it('getCacheStats 走 v1 统计端点', async () => {
    getMock.mockResolvedValue({
      data: { content: { files: 1, bytes: 2, chapters: 3, versions: 4 } },
    })

    await getCacheStats()

    expect(getMock).toHaveBeenCalledWith('/cache/stats', { baseURL: API_V1_BASE })
  })

  it('purgeCacheByKind 用 DELETE + kind 查询参数', async () => {
    deleteMock.mockResolvedValue({ data: { purged: { cover: 3 } } })

    await purgeCacheByKind('cover')

    expect(deleteMock).toHaveBeenCalledWith('/cache', {
      baseURL: API_V1_BASE,
      params: { kind: 'cover' },
    })
  })

  it('purgeAllUserCache 不带 kind（清全部层）', async () => {
    deleteMock.mockResolvedValue({ data: { purged: {} } })

    await purgeAllUserCache()

    expect(deleteMock).toHaveBeenCalledWith('/cache', { baseURL: API_V1_BASE })
  })
})
