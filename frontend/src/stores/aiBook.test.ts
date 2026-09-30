import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useAiBookStore } from './aiBook'
import { getAiModelStatus } from '../api/aiModel'
import type { AiServerModelStatus } from '../types'

vi.mock('../api/aiModel', () => ({
  getAiModelStatus: vi.fn(),
}))

const getAiModelStatusMock = vi.mocked(getAiModelStatus)

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
