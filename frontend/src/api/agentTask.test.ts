import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import {
  cancelAgentTask,
  getAgentTaskStatus,
  mapTaskToAgentSnapshot,
  runAgentTask,
} from './agentTask'
import { API_V1_BASE } from '../utils/appBase'
import type { components } from './generated'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
    delete: vi.fn(),
  },
}))

const getMock = http.get as unknown as Mock
const postMock = http.post as unknown as Mock
const deleteMock = http.delete as unknown as Mock

type ApiTask = components['schemas']['TaskView']

function createTask(overrides: Partial<ApiTask> = {}): ApiTask {
  return {
    id: 'tsk_7',
    kind: 'ai-generate',
    state: 'running',
    progress: {
      done: 3,
      total: 10,
      unit: 'chapter',
      message: '更新第 3 章的 AI 资料...',
      subjectId: 'book_5',
      phase: 'text',
    },
    createdAt: '2026-11-14T00:00:00Z',
    startedAt: '2026-11-14T00:00:01Z',
    finishedAt: null,
    resultHref: null,
    error: null,
    ...overrides,
  }
}

describe('agentTask api（v1）', () => {
  beforeEach(() => {
    getMock.mockReset()
    postMock.mockReset()
    deleteMock.mockReset()
  })

  it('把运行中的 TaskView 映射成 sidecar 快照形状', () => {
    const snapshot = mapTaskToAgentSnapshot(createTask(), 'book_5')

    expect(snapshot).toEqual({
      running: true,
      jobId: 'tsk_7',
      bookId: 'book_5',
      phase: 'text',
      statusText: '更新第 3 章的 AI 资料...',
      // 进度是「第 n 章 / 共 m 章」，1 起；快照里的章节下标是 0 起
      currentChapterIndex: 2,
      targetChapterIndex: 9,
      lastError: null,
    })
  })

  it('失败任务给 error 阶段与 problem detail，不再显示为运行中', () => {
    const snapshot = mapTaskToAgentSnapshot(
      createTask({
        state: 'failed',
        progress: {
          done: 4,
          total: 10,
          unit: 'chapter',
          message: null,
          subjectId: 'book_5',
          phase: 'saving',
        },
        error: {
          type: 'about:blank',
          code: 'UPSTREAM_FAILED',
          status: 502,
          title: 'Upstream Failed',
          detail: '模型服务返回 401 Unauthorized',
        },
      }),
      'book_5'
    )

    expect(snapshot.running).toBe(false)
    expect(snapshot.phase).toBe('error')
    expect(snapshot.lastError).toBe('模型服务返回 401 Unauthorized')
    expect(snapshot.statusText).toBe('')
  })

  it('getAgentTaskStatus 按书取最近任务，仅 TASK_NOT_FOUND 收敛为 null', async () => {
    getMock.mockResolvedValueOnce({ data: createTask() })
    await expect(getAgentTaskStatus('book_5')).resolves.toMatchObject({
      jobId: 'tsk_7',
      bookId: 'book_5',
    })
    expect(getMock).toHaveBeenCalledWith('/books/book_5/agent-task', {
      baseURL: API_V1_BASE,
    })

    getMock.mockRejectedValueOnce(
      Object.assign(new Error('not found'), {
        status: 404,
        code: 'TASK_NOT_FOUND',
      })
    )
    await expect(getAgentTaskStatus('book_5')).resolves.toBeNull()

    // 书籍不存在是真错误，不能当成「没有任务」静默吞掉
    getMock.mockRejectedValueOnce(
      Object.assign(new Error('书籍不存在'), {
        status: 404,
        code: 'BOOK_NOT_FOUND',
      })
    )
    await expect(getAgentTaskStatus('book_missing')).rejects.toThrow('书籍不存在')
  })

  it('runAgentTask 用 kebab-case kind 提交，只在有目标章时带该字段', async () => {
    postMock.mockResolvedValue({ data: createTask() })

    await runAgentTask({
      bookId: 'book_5',
      kind: 'update-to-current',
      targetChapterIndex: 9,
    })
    expect(postMock).toHaveBeenLastCalledWith(
      '/books/book_5/agent-tasks',
      { kind: 'update-to-current', targetChapterIndex: 9 },
      { baseURL: API_V1_BASE }
    )

    await runAgentTask({ bookId: 'book_5', kind: 'redraw-map' })
    expect(postMock).toHaveBeenLastCalledWith(
      '/books/book_5/agent-tasks',
      { kind: 'redraw-map' },
      { baseURL: API_V1_BASE }
    )
  })

  it('cancelAgentTask 走定点 DELETE', async () => {
    deleteMock.mockResolvedValue({})
    await cancelAgentTask('book_5')
    expect(deleteMock).toHaveBeenCalledWith('/books/book_5/agent-task', {
      baseURL: API_V1_BASE,
    })
  })
})
