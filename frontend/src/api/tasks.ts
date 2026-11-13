import http from './http'
import { API_V1_BASE } from '../utils/appBase'
import { parseSourceId } from './source'
import type { BookSourceTestResponse } from '../types'
import type { components } from './generated'

/** v1 任务资源（字段见 openapi.yaml 的 TaskView）。 */
export type ApiTask = components['schemas']['TaskView']
export type ApiTaskState = components['schemas']['TaskState']
type ApiSearchBook = components['schemas']['SearchBook']
type ApiSourceTestItem = components['schemas']['SourceTestItem']

/** 书源测试任务单次上限（后端 MAX_SOURCE_TASK_ITEMS）。 */
export const MAX_SOURCE_TEST_TASK_SIZE = 1000

export interface TaskProgress {
  done: number
  total: number
  unit: string
  message?: string | null
}

export interface TaskItemEvent {
  items: ApiSearchBook[]
  cursor?: string | null
}

export interface TaskDoneEvent {
  state: ApiTaskState
  resultHref?: string | null
}

/** SSE `error` 帧的 problem+json 数据。 */
export interface TaskProblem {
  code?: string
  status?: number
  detail?: string
  title?: string
}

export interface PrefetchSummary {
  requested: number
  cached: number
  skipped: number
  failed: number
}

export interface SearchTaskResult {
  items: ApiSearchBook[]
  nextCursor?: string | null
  total: number
  truncated?: boolean
}

const TERMINAL_STATES = new Set<ApiTaskState>(['succeeded', 'failed', 'cancelled'])

export function isTerminalTaskState(state: string): boolean {
  return TERMINAL_STATES.has(state as ApiTaskState)
}

/**
 * 创建多源搜索任务。`sourceIds` 与 `groupId` 二选一；都省略表示全部启用源。
 */
export function createSearchTask(params: {
  q: string
  sourceIds?: string[]
  groupId?: string
  limitPerSource?: number
}): Promise<ApiTask> {
  return http
    .post<ApiTask>(
      '/searches',
      {
        q: params.q,
        ...(params.sourceIds ? { sourceIds: params.sourceIds } : {}),
        ...(params.groupId ? { groupId: params.groupId } : {}),
        ...(params.limitPerSource ? { limitPerSource: params.limitPerSource } : {}),
      },
      { baseURL: API_V1_BASE }
    )
    .then((r) => r.data)
}

/** 预下载章节到服务端正文池（from 为章节下标）。 */
export function createPrefetchTask(
  bookId: string,
  params: { from: number; count: number }
): Promise<ApiTask> {
  return http
    .post<ApiTask>(`/books/${bookId}/cache`, params, { baseURL: API_V1_BASE })
    .then((r) => r.data)
}

/** 创建书源测试任务（ids 是 `src_<n>`；markInvalid 默认 false）。 */
export function createSourceTestTask(params: {
  ids: string[]
  keyword?: string
  markInvalid?: boolean
}): Promise<ApiTask> {
  return http
    .post<ApiTask>(
      '/sources:test',
      {
        ids: params.ids,
        ...(params.keyword ? { keyword: params.keyword } : {}),
      },
      {
        baseURL: API_V1_BASE,
        params: params.markInvalid ? { markInvalid: true } : undefined,
      }
    )
    .then((r) => r.data)
}

/** 书源测试结果 → 旧 UI 形状（SourcesView 的合并/提示逻辑零改动）。 */
export function asSourceTestResult(value: unknown): BookSourceTestResponse | null {
  const result = value as
    | { total?: number; valid?: number; invalid?: number; markedInvalid?: number; results?: unknown }
    | null
  if (!result || !Array.isArray(result.results)) return null
  const results = (result.results as ApiSourceTestItem[]).map((item) => ({
    bookSourceId: item.sourceId ? parseSourceId(item.sourceId) : undefined,
    bookSourceName: item.name,
    bookSourceUrl: '',
    valid: item.valid,
    searchOk: item.searchOk,
    exploreOk: item.exploreOk,
    keyword: item.keyword,
    searchError: item.searchError ?? undefined,
    exploreError: item.exploreError ?? undefined,
    markedInvalid: item.markedInvalid,
    group: item.group ?? undefined,
  }))
  return {
    total: result.total ?? results.length,
    valid: result.valid ?? 0,
    invalid: result.invalid ?? 0,
    markedInvalid: result.markedInvalid ?? 0,
    results,
  }
}

export function getTask(taskId: string): Promise<ApiTask> {  return http.get<ApiTask>(`/tasks/${taskId}`, { baseURL: API_V1_BASE }).then((r) => r.data)
}

export function cancelTask(taskId: string): Promise<void> {
  return http.delete(`/tasks/${taskId}`, { baseURL: API_V1_BASE }).then(() => undefined)
}

export function getTaskResult(taskId: string, cursor?: string): Promise<unknown> {
  return http
    .get(`/tasks/${taskId}/result`, {
      baseURL: API_V1_BASE,
      params: cursor ? { cursor } : undefined,
    })
    .then((r) => r.data)
}

/** 任务结果的窄化：搜索分页 / 预下载摘要。 */
export function asSearchResult(value: unknown): SearchTaskResult | null {
  const result = value as Partial<SearchTaskResult> | null
  return result && Array.isArray(result.items) ? (result as SearchTaskResult) : null
}

export function asPrefetchSummary(value: unknown): PrefetchSummary | null {
  const result = value as Partial<PrefetchSummary> | null
  return result && typeof result.requested === 'number' ? (result as PrefetchSummary) : null
}

export interface TaskRunHandlers {
  onProgress?: (progress: TaskProgress) => void
  onItem?: (items: ApiSearchBook[], event: TaskItemEvent) => void
  onError?: (problem: TaskProblem) => void
  /** `reconnecting=false` 表示 EventSource 已进入 CLOSED（4xx 不会自动重连） */
  onTransportError?: (reconnecting: boolean) => void
  /** 终态；result 已按 `resultHref` 拉取（失败时为 undefined） */
  onDone?: (state: ApiTaskState, result: unknown) => void
}

export interface TaskRunHandle {
  close: () => void
}

/**
 * 消费任务事件流：
 * - SSE 推送增量（`progress`/`item`/`error`/`done`）；
 * - 2 秒轮询 `GET /v1/tasks/{id}` 兜底：SSE 断线/事件丢包时仍能发现终态；
 * - 浏览器原生 `EventSource` 自动重连带 `Last-Event-ID`；
 * - 主动 `close()`（新搜索/卸载）会 DELETE 取消任务，避免后台白跑。
 */
export function runTask(task: ApiTask, handlers: TaskRunHandlers): TaskRunHandle {
  let settled = false
  let pollTimer: ReturnType<typeof setInterval> | null = null
  const source = new EventSource(`${API_V1_BASE}/tasks/${task.id}/events`)

  const parse = (event: MessageEvent): unknown => {
    try {
      return JSON.parse(event.data)
    } catch {
      return null
    }
  }

  const cleanup = () => {
    if (pollTimer) {
      clearInterval(pollTimer)
      pollTimer = null
    }
    source.close()
  }

  const finalize = async (state: ApiTaskState, resultHref?: string | null) => {
    if (settled) return
    settled = true
    cleanup()
    let result: unknown
    if (resultHref) {
      try {
        result = await getTaskResult(task.id)
      } catch {
        result = undefined
      }
    }
    handlers.onDone?.(state, result)
  }

  /** 任务已从注册表消失/会话失效：没有终态事件可等，直接收敛并让 UI 提示重试。 */
  const taskGone = () => {
    if (settled) return
    settled = true
    cleanup()
    handlers.onError?.({ detail: '任务已失效，请重新搜索' })
    handlers.onDone?.('failed', undefined)
  }

  const statusOf = (error: unknown): number | undefined => {
    const value = error as { status?: unknown } | null
    return typeof value?.status === 'number' ? value.status : undefined
  }

  const treatIfGone = (error: unknown) => {
    const status = statusOf(error)
    if (status === 404 || status === 401) taskGone()
  }

  source.addEventListener('progress', (event) => {
    const data = parse(event as MessageEvent) as TaskProgress | null
    if (data) handlers.onProgress?.(data)
  })
  source.addEventListener('item', (event) => {
    const data = parse(event as MessageEvent) as TaskItemEvent | null
    if (data?.items?.length) handlers.onItem?.(data.items, data)
  })
  source.addEventListener('error', (event) => {
    // 命名 error 帧是业务错误（problem+json）；Event 是传输层失败。
    if (event instanceof MessageEvent) {
      const data = parse(event) as TaskProblem | null
      if (data) handlers.onError?.(data)
      return
    }
    // 4xx（401/404）会让 EventSource 进入 CLOSED 且不再自动重连；网络错误
    // 则是 CONNECTING，由浏览器原生重连带 Last-Event-ID。
    const reconnecting = source.readyState !== 2
    handlers.onTransportError?.(reconnecting)
    if (!reconnecting) {
      void getTask(task.id)
        .then((fresh) => {
          if (settled) return
          if (isTerminalTaskState(fresh.state)) {
            void finalize(fresh.state, fresh.resultHref)
          }
        })
        .catch(treatIfGone)
    }
  })
  source.addEventListener('done', (event) => {
    const data = parse(event as MessageEvent) as TaskDoneEvent | null
    if (data) void finalize(data.state, data.resultHref)
  })

  pollTimer = setInterval(() => {
    if (settled) return
    void getTask(task.id)
      .then((fresh) => {
        if (settled) return
        if (isTerminalTaskState(fresh.state)) {
          void finalize(fresh.state, fresh.resultHref)
        }
      })
      .catch(treatIfGone)
  }, 2000)

  return {
    close: () => {
      if (settled) return
      settled = true
      cleanup()
      void cancelTask(task.id).catch(() => undefined)
    },
  }
}
