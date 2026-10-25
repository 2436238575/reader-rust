import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useAiBookStore } from './aiBook'
import { getAiModelStatus } from '../api/aiModel'
import { getAiBookMemory } from '../api/aiBook'
import { getAgentTaskStatus, runAgentTask } from '../api/agentTask'
import type { AiServerModelStatus, Book } from '../types'

vi.mock('../api/aiModel', () => ({
  getAiModelStatus: vi.fn(),
}))

vi.mock('../api/aiBook', () => ({
  getAiBookMemory: vi.fn(),
  saveAiBookMemory: vi.fn(),
  deleteAiBookMemory: vi.fn(),
}))

vi.mock('../api/agentTask', () => ({
  runAgentTask: vi.fn(),
  cancelAgentTask: vi.fn(),
  getAgentTaskStatus: vi.fn(),
}))

const getAiModelStatusMock = vi.mocked(getAiModelStatus)
const getAiBookMemoryMock = vi.mocked(getAiBookMemory)
const runAgentTaskMock = vi.mocked(runAgentTask)
const getAgentTaskStatusMock = vi.mocked(getAgentTaskStatus)

describe('aiBook store server model status', () => {
  beforeEach(() => {
    installLocalStorage()
    setActivePinia(createPinia())
    getAiModelStatusMock.mockReset()
  })

  it('reuses the loaded server model status for repeated checks', async () => {
    const status = createServerModelStatus()
    getAiModelStatusMock.mockResolvedValue(status)
    const store = useAiBookStore()

    await expect(store.loadServerModelStatus()).resolves.toEqual(status)
    await expect(store.loadServerModelStatus()).resolves.toEqual(status)

    expect(getAiModelStatusMock).toHaveBeenCalledTimes(1)
  })

  it('exposes per-kind readiness flags only', async () => {
    getAiModelStatusMock.mockResolvedValue(createServerModelStatus())
    const store = useAiBookStore()
    await store.loadServerModelStatus()

    expect(store.canUseServerModel).toBe(true)
    expect(store.serverTextReady).toBe(true)
    expect(store.serverImageReady).toBe(false)
    expect(store.serverSpeechReady).toBe(true)
    // 状态里不应出现任何配置本体（地址/Key 等字段名都不下发）
    expect(JSON.stringify(store.serverModelStatus)).not.toMatch(/apiKey|baseUrl/i)
  })
})

function createServerModelStatus(): AiServerModelStatus {
  return {
    canUseServerModel: true,
    textReady: true,
    imageReady: false,
    speechReady: true,
    agentReady: false,
  }
}

describe('aiBook store maybeAutoUpdate memory reuse', () => {
  beforeEach(() => {
    installLocalStorage()
    setActivePinia(createPinia())
    getAiBookMemoryMock.mockReset()
    runAgentTaskMock.mockReset()
    getAgentTaskStatusMock.mockReset()
  })

  it('连续两次翻章只询问一次记忆，且不会启动任务', async () => {
    // 没开 AI 的书：服务端没有记忆，返回 null
    getAiBookMemoryMock.mockResolvedValue(null)
    const store = useAiBookStore()
    const book = createBook()

    await store.maybeAutoUpdate(book, 3)
    await store.maybeAutoUpdate(book, 4)

    // 第一次拿到的空记忆已回写 memory.value：第二次直接命中，不再发请求
    expect(getAiBookMemoryMock).toHaveBeenCalledTimes(1)
    expect(store.memory?.bookId).toBe(book.bookId)
    expect(store.memory?.enabled).toBe(false)
    // 没开 AI 不触发任务，也不启动轮询
    expect(runAgentTaskMock).not.toHaveBeenCalled()
    expect(getAgentTaskStatusMock).not.toHaveBeenCalled()
  })

  it('记忆请求失败保持静默，不写回本地记忆', async () => {
    getAiBookMemoryMock.mockRejectedValue(new Error('network'))
    const store = useAiBookStore()

    await expect(store.maybeAutoUpdate(createBook(), 3)).resolves.toBeUndefined()

    expect(store.memory).toBeNull()
    expect(runAgentTaskMock).not.toHaveBeenCalled()
  })
})

function createBook(): Book {
  return {
    bookId: 'book_1',
    name: '测试书',
    author: '作者',
    bookUrl: 'https://example.test/book',
    origin: 'https://example.test',
  }
}

function installLocalStorage() {
  const memory = new Map<string, string>()
  Object.defineProperty(globalThis, 'localStorage', {
    value: {
      getItem: (key: string) => memory.get(key) || null,
      setItem: (key: string, value: string) => memory.set(key, value),
      removeItem: (key: string) => memory.delete(key),
      clear: () => memory.clear(),
    },
    configurable: true,
  })
}
