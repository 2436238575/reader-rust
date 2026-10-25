// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import axios from 'axios'
import { createAuthedEventSource } from './authedEventSource'
import { AUTH_HINT_STORAGE_KEY } from './secureAccess'

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

/** 写入非敏感登录提示；会话凭据都在 HttpOnly Cookie 里，测试无法也无需伪造。 */
function seedHint(accessTokenExpiresAt: number) {
  localStorage.setItem(
    AUTH_HINT_STORAGE_KEY,
    JSON.stringify({ username: 'reader1', accessTokenExpiresAt })
  )
}

async function flushMicrotasks() {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

// authSession 的刷新有 30 秒成功短路窗口（模块级时间戳）：每个用例结束时
// 把假时钟拨过窗口，避免上一个用例的刷新把下一个用例短路掉
let fakeNow = new Date('2026-01-01T00:00:00Z').getTime()

beforeEach(() => {
  localStorage.clear()
  FakeEventSource.instances = []
  vi.stubGlobal('EventSource', FakeEventSource)
  vi.useFakeTimers()
  vi.setSystemTime(fakeNow)
})

afterEach(() => {
  fakeNow = Date.now() + 120_000
  vi.useRealTimers()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  localStorage.clear()
})

describe('authedEventSource', () => {
  it('URL 不携带任何令牌，会话临近过期时刷新后重连', async () => {
    seedHint(Date.now() - 1000) // hint 里的过期时间已过
    const post = vi.spyOn(axios, 'post').mockResolvedValue({
      data: { isSuccess: true, errorMsg: '', data: { accessTokenExpiresIn: 21600 } },
    })

    const stream = createAuthedEventSource('/searchBookMultiSSE', { key: 'x' })
    expect(FakeEventSource.instances).toHaveLength(1)
    expect(FakeEventSource.instances[0].url).not.toContain('accessToken')

    FakeEventSource.instances[0].emit('error', new Event('error'))
    await vi.advanceTimersByTimeAsync(500)
    await flushMicrotasks()

    expect(post).toHaveBeenCalledWith(
      expect.stringContaining('/refreshToken'),
      expect.anything(),
      expect.objectContaining({ withCredentials: true })
    )
    expect(FakeEventSource.instances).toHaveLength(2)
    expect(FakeEventSource.instances[0].closed).toBe(true)
    expect(FakeEventSource.instances[1].url).not.toContain('accessToken')
    stream.close()
    expect(FakeEventSource.instances[1].closed).toBe(true)
  })

  it('会话新鲜时首次失败交给原生重连，连续第二次失败才刷新', async () => {
    seedHint(Date.now() + 3_600_000)
    const post = vi.spyOn(axios, 'post').mockResolvedValue({
      data: { isSuccess: true, errorMsg: '', data: { accessTokenExpiresIn: 21600 } },
    })

    createAuthedEventSource('/searchBookMultiSSE', { key: 'x' })
    FakeEventSource.instances[0].emit('error', new Event('error'))
    await vi.advanceTimersByTimeAsync(5_000)
    expect(FakeEventSource.instances).toHaveLength(1)
    expect(post).not.toHaveBeenCalled()

    FakeEventSource.instances[0].emit('error', new Event('error'))
    await vi.advanceTimersByTimeAsync(500)
    await flushMicrotasks()

    expect(post).toHaveBeenCalledTimes(1)
    expect(FakeEventSource.instances).toHaveLength(2)
  })

  it('刷新被 401 拒绝时关闭连接并停止重试', async () => {
    seedHint(Date.now() - 1000)
    const post = vi
      .spyOn(axios, 'post')
      .mockRejectedValue({ response: { status: 401 } })

    const stream = createAuthedEventSource('/searchBookMultiSSE', { key: 'x' })
    FakeEventSource.instances[0].emit('error', new Event('error'))
    await vi.advanceTimersByTimeAsync(10_000)
    await flushMicrotasks()

    expect(post).toHaveBeenCalledTimes(1)
    expect(FakeEventSource.instances).toHaveLength(1)
    expect(FakeEventSource.instances[0].closed).toBe(true)
    // 会话失效时 hint 已被刷新逻辑清空（登录通知由 authSession 统一广播）
    expect(localStorage.getItem(AUTH_HINT_STORAGE_KEY)).toBeNull()
    stream.close()
  })

  it('刷新网络失败时按退避节奏重试，超过上限后停止', async () => {
    seedHint(Date.now() - 1000)
    const post = vi.spyOn(axios, 'post').mockRejectedValue(new Error('network down'))

    const stream = createAuthedEventSource('/searchBookMultiSSE', { key: 'x' })
    FakeEventSource.instances[0].emit('error', new Event('error'))
    // 退避链 400ms + 2s + 4s + 8s 全部走完，共 4 次刷新尝试（上限 3 次重试）
    await vi.advanceTimersByTimeAsync(15_000)
    await flushMicrotasks()
    expect(post).toHaveBeenCalledTimes(4)

    // 再等一段时间：重试已达上限，不再发起新的刷新
    await vi.advanceTimersByTimeAsync(10_000)
    expect(post).toHaveBeenCalledTimes(4)
    expect(FakeEventSource.instances).toHaveLength(1)
    stream.close()
  })

  it('close 之后不再刷新重连', async () => {
    seedHint(Date.now() - 1000)
    const post = vi.spyOn(axios, 'post').mockResolvedValue({
      data: { isSuccess: true, errorMsg: '', data: { accessTokenExpiresIn: 21600 } },
    })

    const stream = createAuthedEventSource('/cacheBookSSE', { bookId: 'book_1' })
    stream.close()
    FakeEventSource.instances[0].emit('error', new Event('error'))
    await vi.advanceTimersByTimeAsync(5_000)
    await flushMicrotasks()

    expect(post).not.toHaveBeenCalled()
    expect(FakeEventSource.instances).toHaveLength(1)
  })

  it('重连后监听器与 onmessage 被重放到新连接，removeEventListener 生效', async () => {
    seedHint(Date.now() - 1000)
    vi.spyOn(axios, 'post').mockResolvedValue({
      data: { isSuccess: true, errorMsg: '', data: { accessTokenExpiresIn: 21600 } },
    })

    const stream = createAuthedEventSource('/searchBookMultiSSE', { key: 'x' })
    const onDelta = vi.fn()
    stream.addEventListener('delta', onDelta)
    const onMessage = vi.fn()
    stream.onmessage = onMessage

    FakeEventSource.instances[0].emit('error', new Event('error'))
    await vi.advanceTimersByTimeAsync(500)
    await flushMicrotasks()

    expect(FakeEventSource.instances).toHaveLength(2)
    expect(FakeEventSource.instances[1].onmessage).toBe(onMessage)
    FakeEventSource.instances[1].emit('delta', new Event('delta'))
    expect(onDelta).toHaveBeenCalledTimes(1)

    // 去重：移除后新连接不再收到该事件
    stream.removeEventListener('delta', onDelta)
    FakeEventSource.instances[1].emit('delta', new Event('delta'))
    expect(onDelta).toHaveBeenCalledTimes(1)
    stream.close()
  })
})
