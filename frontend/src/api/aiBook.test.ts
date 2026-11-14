import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import { deleteAiBookMemory, getAiBookMemory, saveAiBookMemory } from './aiBook'
import { API_V1_BASE } from '../utils/appBase'
import type { AiBookMemory } from '../types'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    put: vi.fn(),
    delete: vi.fn(),
  },
}))

const getMock = http.get as unknown as Mock
const putMock = http.put as unknown as Mock
const deleteMock = http.delete as unknown as Mock

function createMemory(): AiBookMemory {
  return {
    bookId: 'book_5',
    bookUrl: 'https://example.test/book/5',
    bookName: '测试书',
    author: '作者',
    enabled: true,
    updatedAt: 1,
    summary: '总览',
    worldview: [],
    characters: [],
    relationships: [],
    locations: [],
    map: null,
    mapDirty: false,
  }
}

describe('aiBook api（v1）', () => {
  beforeEach(() => {
    getMock.mockReset()
    putMock.mockReset()
    deleteMock.mockReset()
  })

  it('getAiBookMemory 走 v1 路径，仅 AI_MEMORY_NOT_FOUND 收敛为 null', async () => {
    getMock.mockResolvedValueOnce({ data: createMemory() })
    await expect(getAiBookMemory('book_5')).resolves.toMatchObject({
      bookId: 'book_5',
      summary: '总览',
    })
    expect(getMock).toHaveBeenCalledWith('/books/book_5/ai-memory', {
      baseURL: API_V1_BASE,
    })

    getMock.mockRejectedValueOnce(
      Object.assign(new Error('还没有 AI 资料'), {
        status: 404,
        code: 'AI_MEMORY_NOT_FOUND',
      })
    )
    await expect(getAiBookMemory('book_5')).resolves.toBeNull()

    getMock.mockRejectedValueOnce(
      Object.assign(new Error('书籍不存在'), {
        status: 404,
        code: 'BOOK_NOT_FOUND',
      })
    )
    await expect(getAiBookMemory('book_missing')).rejects.toThrow('书籍不存在')
  })

  it('saveAiBookMemory 用 bookId 路径整体替换', async () => {
    const memory = createMemory()
    putMock.mockResolvedValue({ data: memory })
    await expect(saveAiBookMemory(memory)).resolves.toMatchObject({
      bookId: 'book_5',
    })
    expect(putMock).toHaveBeenCalledWith('/books/book_5/ai-memory', memory, {
      baseURL: API_V1_BASE,
    })
  })

  it('deleteAiBookMemory 走幂等 DELETE', async () => {
    deleteMock.mockResolvedValue({})
    await deleteAiBookMemory('book_5')
    expect(deleteMock).toHaveBeenCalledWith('/books/book_5/ai-memory', {
      baseURL: API_V1_BASE,
    })
  })
})
