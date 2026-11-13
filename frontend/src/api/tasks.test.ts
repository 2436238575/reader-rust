import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import {
  asPrefetchSummary,
  asSearchResult,
  asSourceTestResult,
  cancelTask,
  createPrefetchTask,
  createSearchTask,
  createSourceTestTask,
  getTask,
  getTaskResult,
  runTask,
  type ApiTask,
} from './tasks'
import { API_V1_BASE } from '../utils/appBase'

vi.mock('./http', () => ({
  default: {
    post: vi.fn(),
    get: vi.fn(),
    delete: vi.fn(),
  },
}))

const postMock = http.post as unknown as Mock
const getMock = http.get as unknown as Mock
const deleteMock = http.delete as unknown as Mock

const task = {
  id: 'tsk_1',
  kind: 'search',
  state: 'running',
  progress: { done: 0, total: 2, unit: 'source', message: null },
  createdAt: '2026-11-14T00:00:00Z',
  startedAt: null,
  finishedAt: null,
  resultHref: null,
  error: null,
} as unknown as ApiTask

class FakeEventSource {
  static instances: FakeEventSource[] = []
  url: string
  closed = false
  private listeners = new Map<string, Set<(event: Event) => void>>()

  constructor(url: string) {
    this.url = url
    FakeEventSource.instances.push(this)
  }

  addEventListener(type: string, listener: EventListenerOrEventListenerObject) {
    const set = this.listeners.get(type) ?? new Set()
    const fn = typeof listener === 'function' ? listener : listener.handleEvent.bind(listener)
    set.add(fn)
    this.listeners.set(type, set)
  }

  close = vi.fn(() => {
    this.closed = true
  })

  emit(type: string, data?: unknown, named = true) {
    const event = named
      ? new MessageEvent(type, { data: JSON.stringify(data) })
      : new Event(type)
    for (const listener of this.listeners.get(type) ?? []) listener(event)
  }
}

beforeEach(() => {
  postMock.mockReset()
  getMock.mockReset()
  deleteMock.mockReset()
  FakeEventSource.instances = []
  vi.stubGlobal('EventSource', FakeEventSource)
})

describe('tasks API（v1 任务模型）', () => {
  it('createSearchTask 按范围二选一上送 sourceIds / groupId', async () => {
    postMock.mockResolvedValue({ data: task })

    await createSearchTask({ q: '三体', sourceIds: ['src_2'] })
    expect(postMock).toHaveBeenLastCalledWith(
      '/searches',
      { q: '三体', sourceIds: ['src_2'] },
      { baseURL: API_V1_BASE }
    )

    await createSearchTask({ q: '三体', groupId: '玄幻' })
    expect(postMock).toHaveBeenLastCalledWith(
      '/searches',
      { q: '三体', groupId: '玄幻' },
      { baseURL: API_V1_BASE }
    )
  })

  it('createPrefetchTask / getTask / result / cancel 走任务路径', async () => {
    postMock.mockResolvedValue({ data: task })
    await createPrefetchTask('book_1', { from: 3, count: 10 })
    expect(postMock).toHaveBeenCalledWith(
      '/books/book_1/cache',
      { from: 3, count: 10 },
      { baseURL: API_V1_BASE }
    )

    getMock.mockResolvedValue({ data: task })
    await getTask('tsk_1')
    expect(getMock).toHaveBeenCalledWith('/tasks/tsk_1', { baseURL: API_V1_BASE })

    getMock.mockResolvedValue({ data: { items: [], total: 0 } })
    await getTaskResult('tsk_1')
    expect(getMock).toHaveBeenCalledWith('/tasks/tsk_1/result', {
      baseURL: API_V1_BASE,
      params: undefined,
    })

    deleteMock.mockResolvedValue({ data: undefined })
    await cancelTask('tsk_1')
    expect(deleteMock).toHaveBeenCalledWith('/tasks/tsk_1', { baseURL: API_V1_BASE })
  })

  it('结果窄化按形状区分搜索分页与预下载摘要', () => {
    expect(asSearchResult({ items: [], total: 0 })?.total).toBe(0)
    expect(asSearchResult({ requested: 1 })).toBeNull()
    expect(asPrefetchSummary({ requested: 2, cached: 1, skipped: 0, failed: 1 })).toEqual({
      requested: 2,
      cached: 1,
      skipped: 0,
      failed: 1,
    })
    expect(asPrefetchSummary({ items: [] })).toBeNull()
  })

  it('runTask 消费 progress/item/done，并按 resultHref 拉结果', async () => {
    getMock.mockResolvedValue({
      data: { items: [{ bookId: 'book_1' }], total: 1 },
    })
    const onProgress = vi.fn()
    const onItem = vi.fn()
    const onDone = vi.fn()

    const handle = runTask(task, { onProgress, onItem, onDone })
    const source = FakeEventSource.instances[0]
    expect(source.url).toContain('/v1/tasks/tsk_1/events')

    source.emit('progress', { done: 1, total: 2, unit: 'source', message: '1/2' })
    source.emit('item', { items: [{ bookId: 'book_1' }], cursor: 'c1' })
    source.emit('done', {
      state: 'succeeded',
      resultHref: '/v1/tasks/tsk_1/result',
    })
    await new Promise((resolve) => setTimeout(resolve, 0))

    expect(onProgress).toHaveBeenCalledWith(
      expect.objectContaining({ done: 1, total: 2 })
    )
    expect(onItem).toHaveBeenCalledWith([{ bookId: 'book_1' }], expect.anything())
    expect(onDone).toHaveBeenCalledWith('succeeded', expect.objectContaining({ total: 1 }))
    expect(source.closed).toBe(true)
    handle.close()
  })

  it('主动 close 会取消任务', () => {
    deleteMock.mockResolvedValue({ data: undefined })
    const handle = runTask(task, {})
    handle.close()
    expect(deleteMock).toHaveBeenCalledWith('/tasks/tsk_1', { baseURL: API_V1_BASE })
  })

  it('任务 404/401 时收敛为 failed，不会永远停在重连中', async () => {
    vi.useFakeTimers()
    try {
      getMock.mockRejectedValue({ status: 404 })
      const onDone = vi.fn()
      const onError = vi.fn()
      const handle = runTask(task, { onDone, onError })

      await vi.advanceTimersByTimeAsync(2100)

      expect(onError).toHaveBeenCalledWith(
        expect.objectContaining({ detail: '任务已失效，请重新搜索' })
      )
      expect(onDone).toHaveBeenCalledWith('failed', undefined)
      // 已收敛：close 不再触发取消
      deleteMock.mockClear()
      handle.close()
      expect(deleteMock).not.toHaveBeenCalled()
    } finally {
      vi.useRealTimers()
    }
  })

  it('createSourceTestTask 用 src_ id 建任务，结果映射回旧 UI 形状', async () => {
    postMock.mockResolvedValue({ data: task })
    await createSourceTestTask({ ids: ['src_1'], markInvalid: true })
    expect(postMock).toHaveBeenCalledWith(
      '/sources:test',
      { ids: ['src_1'] },
      { baseURL: API_V1_BASE, params: { markInvalid: true } }
    )

    const summary = asSourceTestResult({
      total: 1,
      valid: 0,
      invalid: 1,
      markedInvalid: 1,
      results: [
        {
          sourceId: 'src_1',
          name: '坏源',
          valid: false,
          searchOk: false,
          exploreOk: false,
          keyword: '测试',
          searchError: 'boom',
          exploreError: null,
          markedInvalid: true,
          group: '失效',
        },
      ],
    })
    expect(summary).toMatchObject({
      total: 1,
      invalid: 1,
      results: [
        {
          bookSourceId: 1,
          bookSourceName: '坏源',
          bookSourceUrl: '',
          searchError: 'boom',
          group: '失效',
        },
      ],
    })
  })
})
