import http from './http'

/**
 * AI 资料编排任务（后端 Python sidecar 承载，前端只触发 + 轮询）。
 *
 * 任务在服务端跑：关页面不取消，重开页面轮询即恢复进度显示。
 * 结果不设独立端点——完成后复用 getAiBookMemory 重拉资料。
 */

export type AgentTaskKind = 'update_to_current' | 'redraw_map'

export type AgentTaskPhase = 'idle' | 'loading' | 'text' | 'map' | 'saving' | 'error'

export interface AgentTaskSnapshot {
  running: boolean
  jobId: string
  bookUrl: string
  phase: AgentTaskPhase
  statusText: string
  currentChapterIndex: number | null
  targetChapterIndex: number | null
  lastError: string | null
}

export function runAgentTask(params: {
  bookUrl: string
  kind: AgentTaskKind
  targetChapterIndex?: number
}) {
  return http.post<{ jobId: string }>('/runAgentTask', params).then((r) => r.data)
}

export function getAgentTaskStatus() {
  return http.post<AgentTaskSnapshot>('/getAgentTaskStatus').then((r) => r.data)
}

export function cancelAgentTask() {
  return http.post<{ cancelled: boolean }>('/cancelAgentTask').then((r) => r.data)
}
