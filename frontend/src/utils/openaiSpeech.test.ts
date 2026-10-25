import { afterEach, describe, expect, it, vi } from 'vitest'
import axios from 'axios'
import { requestOpenAISpeechAudio } from './openaiSpeech'

afterEach(() => {
  vi.restoreAllMocks()
})

describe('openaiSpeech', () => {
  it('routes server configured speech through aiProxy with session cookies', async () => {
    installLocalStorage()
    const fetchMock = vi.fn(async (_url: RequestInfo | URL, _init?: RequestInit) => ({
      ok: true,
      blob: async () => new Blob(['audio'], { type: 'audio/mpeg' }),
      headers: new Headers(),
    }))
    vi.stubGlobal('fetch', fetchMock)

    const blob = await requestOpenAISpeechAudio({
      source: 'server',
      baseUrl: '',
      apiKey: 'browser-key',
      input: '你好',
      model: 'browser-model',
      voice: 'browser-voice',
      format: 'mp3',
      speed: 1,
    })

    expect(blob.type).toBe('audio/mpeg')
    expect(fetchMock).toHaveBeenCalledWith(
      '/reader3/aiProxy',
      expect.objectContaining({
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
      })
    )
    const init = fetchMock.mock.calls[0]?.[1] as unknown as RequestInit
    // 会话在 HttpOnly Cookie 里：请求头不能出现任何令牌
    expect(init.headers).not.toHaveProperty('Authorization')
    expect(JSON.parse(String(init.body))).toMatchObject({
      useServerConfig: true,
      kind: 'speech',
      path: '/v1/audio/speech',
      body: {
        input: '你好',
        response_format: 'mp3',
        speed: 1,
      },
    })
    expect(String(init.body)).not.toContain('browser-key')
  })

  it('refreshes once and retries an expired server speech request', async () => {
    installLocalStorage()
    const post = vi.spyOn(axios, 'post').mockResolvedValue({
      data: {
        isSuccess: true,
        errorMsg: '',
        data: {
          accessToken: 'new-token',
          accessTokenExpiresIn: 21600,
        },
      },
    })
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce({
        ok: false,
        status: 401,
        headers: new Headers(),
        json: async () => ({}),
        text: async () => 'unauthorized',
      })
      .mockResolvedValueOnce({
        ok: true,
        status: 200,
        headers: new Headers(),
        blob: async () => new Blob(['audio'], { type: 'audio/mpeg' }),
      })
    vi.stubGlobal('fetch', fetchMock)

    const blob = await requestOpenAISpeechAudio({
      source: 'server',
      baseUrl: '',
      input: '你好',
      model: 'server-model',
      voice: 'server-voice',
    })

    expect(blob.type).toBe('audio/mpeg')
    expect(post).toHaveBeenCalledTimes(1)
    expect(fetchMock).toHaveBeenCalledTimes(2)
    const retryInit = fetchMock.mock.calls[1]?.[1] as unknown as RequestInit
    // 刷新后重试仍靠 Cookie，不能在请求头里出现令牌
    expect(retryInit.headers).not.toHaveProperty('Authorization')
    expect(retryInit.credentials).toBe('same-origin')
  })
})

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
