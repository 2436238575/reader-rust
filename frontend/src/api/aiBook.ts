import http from './http'
import type { AiBookMemory } from '../types'
import { API_V1_BASE } from '../utils/appBase'

/**
 * 书籍 AI 资料（v1）：按 bookId 读、整体替换写、幂等删。
 *
 * 尚未生成资料时 GET 返回 404 `AI_MEMORY_NOT_FOUND`，这里收敛成 null，
 * 调用方（AI 资料页）用空骨架渲染，不需要区分「没有」与「加载失败」。
 */
export function getAiBookMemory(bookId: string) {
  return http
    .get<AiBookMemory>(`/books/${bookId}/ai-memory`, { baseURL: API_V1_BASE })
    .then((r) => r.data)
    .catch((error: unknown) => {
      // 只把「还没生成」当成空资料；书籍不存在（BOOK_NOT_FOUND）是真错误，
      // 不能让页面安静地显示成空骨架。
      if (isProblem(error, 404, 'AI_MEMORY_NOT_FOUND')) return null
      throw error
    })
}

export function saveAiBookMemory(memory: AiBookMemory) {
  if (!memory.bookId) return Promise.reject(new Error('书籍缺少 bookId'))
  return http
    .put<AiBookMemory>(`/books/${memory.bookId}/ai-memory`, memory, {
      baseURL: API_V1_BASE,
    })
    .then((r) => r.data)
}

export function deleteAiBookMemory(bookId: string) {
  return http
    .delete(`/books/${bookId}/ai-memory`, { baseURL: API_V1_BASE })
    .then(() => undefined)
}

function isProblem(error: unknown, status: number, code: string) {
  if (!error || typeof error !== 'object') return false
  const problem = error as { status?: number; code?: string }
  return problem.status === status && problem.code === code
}
