import http from './http'
import { API_V1_BASE } from '../utils/appBase'
import type { components } from './generated'

/**
 * AI 资料编排任务（后端 Python sidecar 承载，前端只触发 + 轮询）。
 *
 * v1 把 sidecar 快照接进统一任务注册表：POST 返回 `TaskView`，状态查询
 * 按书取最近一次任务（服务重启后任务随内存注册表消失，GET 404 视作无任务）。
 * 任务在服务端跑：关页面不取消，重开页面轮询即恢复进度显示。
 * 结果不设独立端点——完成后复用 getAiBookMemory 重拉资料。
 */

export type AgentTaskKind = 'update-to-current' | 'redraw-map'

export type AgentTaskPhase = 'idle' | 'loading' | 'text' | 'map' | 'saving' | 'error'

export interface AgentTaskSnapshot {
  running: boolean
  jobId: string
  bookId: string
  phase: AgentTaskPhase
  statusText: string
  currentChapterIndex: number | null
  targetChapterIndex: number | null
  lastError: string | null
}

type ApiTask = components['schemas']['TaskView']

const ACTIVE_STATES = new Set(['queued', 'running', 'cancelling'])
const KNOWN_PHASES = new Set<AgentTaskPhase>([
  'idle',
  'loading',
  'text',
  'map',
  'saving',
  'error',
])

/** 任务进度 → 视图沿用的 sidecar 快照形状（章节下标从 0 起）。 */
export function mapTaskToAgentSnapshot(
  task: ApiTask,
  fallbackBookId: string
): AgentTaskSnapshot {
  const running = ACTIVE_STATES.has(task.state)
  const phase = task.state === 'failed'
    ? 'error'
    : task.state === 'succeeded' || task.state === 'cancelled'
      ? 'idle'
      : normalizePhase(task.progress.phase)
  const done = task.progress.done ?? 0
  const total = task.progress.total ?? 0
  return {
    running,
    jobId: task.id,
    bookId: task.progress.subjectId ?? fallbackBookId,
    phase,
    statusText: task.progress.message ?? '',
    currentChapterIndex: done > 0 ? done - 1 : null,
    targetChapterIndex: total > 0 ? total - 1 : null,
    lastError: task.error?.detail ?? null,
  }
}

function normalizePhase(phase: string | null | undefined): AgentTaskPhase {
  return phase && KNOWN_PHASES.has(phase as AgentTaskPhase)
    ? (phase as AgentTaskPhase)
    : 'loading'
}

export function runAgentTask(params: {
  bookId: string
  kind: AgentTaskKind
  targetChapterIndex?: number
}) {
  return http
    .post<ApiTask>(
      `/books/${params.bookId}/agent-tasks`,
      {
        kind: params.kind,
        ...(params.targetChapterIndex != null
          ? { targetChapterIndex: params.targetChapterIndex }
          : {}),
      },
      { baseURL: API_V1_BASE }
    )
    .then((r) => r.data)
}

/** 最近一次任务；没有任务（或服务重启后任务已丢）时返回 null。 */
export async function getAgentTaskStatus(bookId: string) {
  try {
    const task = await http
      .get<ApiTask>(`/books/${bookId}/agent-task`, { baseURL: API_V1_BASE })
      .then((r) => r.data)
    return mapTaskToAgentSnapshot(task, bookId)
  } catch (error) {
    // 只把「这本书还没有任务」当成空状态；书籍不存在要显式报错
    if (isProblem(error, 404, 'TASK_NOT_FOUND')) return null
    throw error
  }
}

export function cancelAgentTask(bookId: string) {
  return http
    .delete(`/books/${bookId}/agent-task`, { baseURL: API_V1_BASE })
    .then(() => undefined)
}

function isProblem(error: unknown, status: number, code: string) {
  if (!error || typeof error !== 'object') return false
  const problem = error as { status?: number; code?: string }
  return problem.status === status && problem.code === code
}
