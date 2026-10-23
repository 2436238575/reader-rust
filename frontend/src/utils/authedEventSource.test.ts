// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import axios from 'axios'
import { createAuthedEventSource } from './authedEventSource'
import { ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY } from './secureAccess'

type Listener = EventListenerOrEventListenerObject

class FakeEventSource {
  static instances: FakeEventSource[] = []
  readonly url: string
  onmessage: ((this: EventSource, ev: MessageEvent) => unknown) | null = null
  onerror: ((this: EventSource, ev: Event) => unknown) | null = null
  readyState = 0
  closed = false
  private readonly listeners = new Map<string, Listener[]>()

  constructor(url: string) {
    this.url = url
    FakeEventSource.instances.push(this)
  }

  addEventListener(type: string, listener: Listener) {
    const list = this.listeners.get(type) ?? []
    list.push(listener)
    this.listeners.set(type, list)
  }

  removeEventListener(type: string, listener: Listener) {
    const list = this.listeners.get(type)
    if (!list) return
    this.listeners.set(
      type,
      list.filter((entry) => entry !== listener)
    )
  }

  close() {
    this.closed = true
    this.readyState = 2
  }

  emit(type: string, event: Event) {
    for (const listener of this.listeners.get(type) ?? []) {
      if (typeof listener === 'function') listener(event)
      else listener.handleEvent(event)
    }
  }
}

function seedExpiredTokens() {
  localStorage.setItem('accessToken', 'old-token')
  localStorage.setItem('refreshToken', 'rt_old')
  localStorage.setItem(ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY, String(Date.now() - 1000))
}

async function flushMicrotasks() {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

beforeEach(() => {
  localStorage.clear()
  FakeEventSource.instances = []
  vi.stubGlobal('EventSource', FakeEventSource)
  vi.useFakeTimers()
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  localStorage.clear()
})

describe('authedEventSource', () => {
  it('rebuilds the stream with a fresh token when the old one has expired', async () => {
    seedExpiredTokens()
    vi.spyOn(axios, 'post').mockResolvedValue({
      data: {
        isSuccess: true,
        errorMsg: '',
        data: {
          accessToken: 'new-token',
          refreshToken: 'rt_new',
          accessTokenExpiresIn: 21600,
        },
      },
    })

    const stream = createAuthedEventSource('/searchBookMultiSSE', { key: 'x' })
    expect(FakeEventSource.instances).toHaveLength(1)
    expect(FakeEventSource.instances[0].url).toContain('accessToken=old-token')

    FakeEventSource.instances[0].emit('error', new Event('error'))
    await vi.advanceTimersByTimeAsync(500)
    await flushMicrotasks()

    expect(FakeEventSource.instances).toHaveLength(2)
    expect(FakeEventSource.instances[0].closed).toBe(true)
    expect(FakeEventSource.instances[1].url).toContain('accessToken=new-token')
    stream.close()
    expect(FakeEventSource.instances[1].closed).toBe(true)
  })

  it('leaves a fresh token to the native reconnection instead of rotating', async () => {
    localStorage.setItem('accessToken', 'fresh-token')
    localStorage.setItem('refreshToken', 'rt_current')
    localStorage.setItem(ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY, String(Date.now() + 3_600_000))
    const post = vi.spyOn(axios, 'post')

    createAuthedEventSource('/searchBookMultiSSE', { key: 'x' })
    FakeEventSource.instances[0].emit('error', new Event('error'))
    await vi.advanceTimersByTimeAsync(5_000)

    expect(FakeEventSource.instances).toHaveLength(1)
    expect(post).not.toHaveBeenCalled()
  })

  it('does not reconnect after close', async () => {
    seedExpiredTokens()
    vi.spyOn(axios, 'post').mockResolvedValue({
      data: {
        isSuccess: true,
        errorMsg: '',
        data: {
          accessToken: 'new-token',
          refreshToken: 'rt_new',
          accessTokenExpiresIn: 21600,
        },
      },
    })

    const stream = createAuthedEventSource('/cacheBookSSE', { bookId: 'book_1' })
    stream.close()
    FakeEventSource.instances[0].emit('error', new Event('error'))
    await vi.advanceTimersByTimeAsync(5_000)
    await flushMicrotasks()

    expect(FakeEventSource.instances).toHaveLength(1)
  })
})
