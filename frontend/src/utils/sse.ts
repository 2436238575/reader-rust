// SSE 统一消费层。
//
// 后端帧约定（book.rs 的 SSE 端点）：
// - 默认 data 帧：JSON 业务载荷
// - 命名 `error` 帧：`{"errorMsg": "用户可读文案"}`（运行期失败，如上游抓取错误）
// - 命名 `end` 帧：JSON 收尾载荷（可能为空）
//
// 坑：后端命名 error 帧与 EventSource 传输层原生 error（断线/HTTP 非 2xx）
// 都是 'error' 类型事件，且同一个 listener 都会收到——靠 `instanceof
// MessageEvent` 区分（原生 error 是 plain Event、无 data）。参数校验失败
// 一律在建流前返回 HTTP 400，不会出现在流里。
//
import type { SseConnection } from './authedEventSource'

// 此前各消费方（SearchResults/useAvailableSources/CacheManager）重复注册
// addEventListener('error') + onerror 且都不读 event.data，后端精心发的
// errorMsg 被丢弃，用户只看到「搜索失败」。统一在此封装。

export interface ConsumeSseHandlers<TMsg = unknown, TEnd = unknown> {
  /** 默认 data 帧（JSON 解析失败时静默跳过） */
  onMessage?: (data: TMsg, event: MessageEvent) => void
  /** 后端 error 帧：errorMsg 是用户可读文案，应展示（toast 或状态条） */
  onErrorMessage?: (errorMsg: string, event: MessageEvent) => void
  /** end 帧；data 为 null 表示空载荷或解析失败 */
  onEnd?: (data: TEnd | null, event: MessageEvent) => void
  /** 传输层失败（断线 / HTTP 非 2xx，如建流前的 400） */
  onTransportError?: (event: Event) => void
}

/** 解析后端 error 帧的 errorMsg；非 error 帧格式返回 null。 */
export function parseSseErrorMsg(raw: string | undefined | null): string | null {
  if (!raw) return null
  try {
    const value = JSON.parse(raw) as { errorMsg?: unknown }
    return typeof value.errorMsg === 'string' && value.errorMsg.trim() ? value.errorMsg : null
  } catch {
    return null
  }
}

/** 解析 JSON 帧；空串/解析失败返回 null。 */
export function parseSseJson<T>(raw: string | undefined | null): T | null {
  if (!raw) return null
  try {
    return JSON.parse(raw) as T
  } catch {
    return null
  }
}

export function consumeSSE<TMsg = unknown, TEnd = unknown>(
  es: SseConnection,
  handlers: ConsumeSseHandlers<TMsg, TEnd>
): SseConnection {
  if (handlers.onMessage) {
    const onMessage = handlers.onMessage
    es.onmessage = (event) => {
      const data = parseSseJson<TMsg>(event.data)
      if (data !== null) onMessage(data, event)
    }
  }
  es.addEventListener('error', (event) => {
    if (event instanceof MessageEvent) {
      const msg = parseSseErrorMsg(event.data) ?? '服务返回了未知错误'
      handlers.onErrorMessage?.(msg, event as MessageEvent<string>)
    } else {
      // 令牌续期与重连由 authedEventSource 负责；这里只把传输层失败交给消费方
      handlers.onTransportError?.(event)
    }
  })
  if (handlers.onEnd) {
    const onEnd = handlers.onEnd
    es.addEventListener('end', (event) => {
      onEnd(parseSseJson<TEnd>((event as MessageEvent).data), event as MessageEvent)
    })
  }
  return es
}
