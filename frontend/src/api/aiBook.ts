import http from './http'
import type { AiBookMemory } from '../types'

export function getAiBookMemory(bookId: string) {
  return http.post<AiBookMemory | null>('/getAiBookMemory', { bookId }).then((r) => r.data)
}

export function saveAiBookMemory(memory: AiBookMemory) {
  return http.post<AiBookMemory>('/saveAiBookMemory', memory).then((r) => r.data)
}

export function deleteAiBookMemory(bookId: string) {
  return http.post<{ deleted: boolean }>('/deleteAiBookMemory', { bookId }).then((r) => r.data)
}
