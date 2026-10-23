// 带鉴权的 EventSource 包装器。
//
// `EventSource` 不能设置请求头，只能把 Access Token 放进查询串。代价是
// URL 在创建时就把令牌固定下来：原生自动重连会一直用旧令牌。这里包装一层，
// 在令牌临近/已经过期且传输失败时，先用 Refresh Token 换新，再用新令牌重建。
import { API_BASE } from './appBase'
import { notifyNeedLogin } from './authEvents'
import { RefreshNetworkError, SessionExpiredError, refreshAccessToken } from './authSession'
import { readAccessToken, readAccessTokenExpiresAt, readRefreshToken } from './secureAccess'

/** SSE 消费方实际用到的最小接口；原生 EventSource 也满足它。 */
export interface SseConnection {
  onmessage: ((this: EventSource, ev: MessageEvent) => unknown) | null
  onerror: ((this: EventSource, ev: Event) => unknown) | null
  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions
  ): void
  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions
  ): void
  close(): void
  readonly readyState: number
}

type ListenerEntry = {
  type: string
  listener: EventListenerOrEventListenerObject
  options?: boolean | AddEventListenerOptions
}

/** 距离过期不足该时长时，传输失败就按「令牌过期」处理。 */
const EXPIRY_MARGIN_MS = 60 * 1000
const MAX_RECONNECT_ATTEMPTS = 3

class AuthedEventSource implements SseConnection {
  private readonly path: string
  private readonly params: Record<string, string | number | undefined>
  private current: EventSource | null = null
  private currentToken?: string
  private readonly listeners: ListenerEntry[] = []
  private messageHandler: ((this: EventSource, ev: MessageEvent) => unknown) | null = null
  private errorHandler: ((this: EventSource, ev: Event) => unknown) | null = null
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null
  private attempts = 0
  private transportErrors = 0
  private closed = false

  constructor(
    path: string,
    params: Record<string, string | number | undefined>
  ) {
    this.path = path
    this.params = params
    this.connect()
  }

  get readyState(): number {
    return this.current?.readyState ?? 2
  }

  get onmessage(): ((this: EventSource, ev: MessageEvent) => unknown) | null {
    return this.messageHandler
  }

  set onmessage(handler: ((this: EventSource, ev: MessageEvent) => unknown) | null) {
    this.messageHandler = handler
    if (this.current) {
      this.current.onmessage = handler as
        | ((this: EventSource, ev: MessageEvent) => unknown)
        | null
    }
  }

  get onerror(): ((this: EventSource, ev: Event) => unknown) | null {
    return this.errorHandler
  }

  set onerror(handler: ((this: EventSource, ev: Event) => unknown) | null) {
    this.errorHandler = handler
    if (this.current) {
      this.current.onerror = handler as ((this: EventSource, ev: Event) => unknown) | null
    }
  }

  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions
  ) {
    this.listeners.push({ type, listener, options })
    this.current?.addEventListener(type, listener, options)
  }

  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions
  ) {
    const index = this.listeners.findIndex(
      (entry) => entry.type === type && entry.listener === listener
    )
    if (index >= 0) this.listeners.splice(index, 1)
    this.current?.removeEventListener(type, listener, options)
  }

  close() {
    this.closed = true
    this.clearReconnectTimer()
    this.current?.close()
    this.current = null
  }

  private connect() {
    const query = new URLSearchParams()
    for (const [key, value] of Object.entries(this.params)) {
      if (value !== undefined) query.set(key, String(value))
    }
    this.currentToken = readAccessToken()
    if (this.currentToken) query.set('accessToken', this.currentToken)

    const source = new EventSource(`${API_BASE}${this.path}?${query.toString()}`)
    this.current = source
    source.onmessage = this.messageHandler as
      | ((this: EventSource, ev: MessageEvent) => unknown)
      | null
    source.onerror = this.errorHandler as ((this: EventSource, ev: Event) => unknown) | null
    source.addEventListener('open', () => {
      if (this.current === source) {
        this.attempts = 0
        this.transportErrors = 0
      }
    })
    source.addEventListener('error', (event) => {
      // 命名 error 帧（MessageEvent）是业务错误，不是传输层失败
      if (this.current !== source || event instanceof MessageEvent) return
      this.onTransportError()
    })
    for (const { type, listener, options } of this.listeners) {
      source.addEventListener(type, listener, options)
    }
  }

  private onTransportError() {
    if (this.closed || this.reconnectTimer) return
    if (!this.tokenLikelyExpired()) {
      // 令牌还新鲜：先交给浏览器原生重连，避免网络抖动也触发轮换。
      // 连续第二次失败再主动换一次，覆盖「服务端换了签名密钥 / 版本号已失效」
      // 但 exp 还没到的情况。
      this.transportErrors += 1
      if (this.transportErrors < 2) return
    }
    this.transportErrors = 0
    if (!readRefreshToken()) {
      notifyNeedLogin()
      this.close()
      return
    }
    this.scheduleReconnect(0)
  }

  private tokenLikelyExpired(): boolean {
    const expiresAt = readAccessTokenExpiresAt()
    return !expiresAt || expiresAt - Date.now() <= EXPIRY_MARGIN_MS
  }

  private scheduleReconnect(attempt: number) {
    if (this.closed || this.reconnectTimer) return
    const delay = attempt === 0 ? 400 : Math.min(1000 * 2 ** attempt, 8000)
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null
      void this.reconnect()
    }, delay)
  }

  private async reconnect() {
    if (this.closed) return
    const staleToken = this.currentToken
    this.current?.close()
    this.current = null
    try {
      await refreshAccessToken(staleToken)
      if (this.closed) return
      this.connect()
      this.attempts = 0
    } catch (error) {
      if (error instanceof SessionExpiredError) {
        this.close()
        return
      }
      // 网络/服务端故障：令牌仍在，退避后继续尝试
      if (
        error instanceof RefreshNetworkError &&
        this.attempts < MAX_RECONNECT_ATTEMPTS &&
        readRefreshToken()
      ) {
        this.attempts += 1
        this.scheduleReconnect(this.attempts)
      }
    }
  }

  private clearReconnectTimer() {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
  }
}

/** 构造带鉴权查询串、并在令牌过期后自动换新重连的 SSE 连接。 */
export function createAuthedEventSource(
  path: string,
  params: Record<string, string | number | undefined>
): SseConnection {
  return new AuthedEventSource(path, params)
}
