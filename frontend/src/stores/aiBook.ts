import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { getAiModelStatus } from '../api/aiModel'
import {
  cancelAgentTask,
  getAgentTaskStatus,
  runAgentTask,
  type AgentTaskPhase,
  type AgentTaskSnapshot,
} from '../api/agentTask'
import { deleteAiBookMemory, getAiBookMemory, saveAiBookMemory } from '../api/aiBook'
import type { AiBookMemory, AiServerModelStatus, Book } from '../types'

/**
 * AI 资料编排（后端 sidecar 承载）：
 * - 生成循环、patch 归一化全部在服务端；前端只做触发、轮询与展示；
 * - 任务在服务端跑，关页面不取消；重开页面 onMounted 里 startPolling 恢复显示；
 * - 快照带 bookId——A 书任务跑着、打开 B 书页面时按钮不受影响。
 */

const POLL_INTERVAL_MS = 2000
const POLL_INTERVAL_HIDDEN_MS = 10000

export const useAiBookStore = defineStore('aiBook', () => {
  const memory = ref<AiBookMemory | null>(null)
  const loading = ref(false)
  // 后端模型只有「是否可用」状态可见（配置本体在服务端 env，不下发）
  const serverModelStatus = ref<AiServerModelStatus | null>(null)
  let serverModelStatusRequest: Promise<AiServerModelStatus | null> | null = null

  async function loadServerModelStatus(options: { force?: boolean } = {}) {
    if (!options.force && serverModelStatus.value) {
      return serverModelStatus.value
    }
    if (!options.force && serverModelStatusRequest) {
      return serverModelStatusRequest
    }

    const request = getAiModelStatus()
      .then((status) => {
        serverModelStatus.value = status
        return status
      })
      .catch(() => {
        serverModelStatus.value = null
        return null
      })
      .finally(() => {
        if (serverModelStatusRequest === request) {
          serverModelStatusRequest = null
        }
      })

    serverModelStatusRequest = request
    return request
  }

  async function load(book: Book) {
    loading.value = true
    try {
      if (!book.bookId) return null
      if (currentBookId.value !== book.bookId) {
        // 换书：旧书的快照/跳变状态不能带到新书页
        task.value = null
        wasRunning.value = false
      }
      currentBookId.value = book.bookId
      const saved = await getAiBookMemory(book.bookId)
      memory.value = saved || emptyMemory(book)
      return memory.value
    } finally {
      loading.value = false
    }
  }

  async function save(next: AiBookMemory) {
    memory.value = await saveAiBookMemory(next)
    return memory.value
  }

  async function setEnabled(book: Book, enabled: boolean) {
    const current = memory.value?.bookId === book.bookId ? memory.value : await load(book)
    if (!current) return null
    return save({
      ...current,
      bookId: book.bookId,
      bookUrl: book.bookUrl,
      bookName: book.name,
      author: book.author,
      enabled,
      updatedAt: Date.now(),
    })
  }

  async function reset(book: Book) {
    if (!book.bookId) return memory.value
    await deleteAiBookMemory(book.bookId)
    memory.value = emptyMemory(book)
    return memory.value
  }

  // -- 任务状态与轮询 --------------------------------------------------------

  const task = ref<AgentTaskSnapshot | null>(null)
  const wasRunning = ref(false)
  // v1 任务按书查询：轮询目标就是当前资料页/阅读器正在看的书
  const currentBookId = ref<string | null>(null)
  let pollTimer: ReturnType<typeof setTimeout> | null = null
  let polling = false

  const isBusy = computed(() => Boolean(task.value?.running))
  const phase = computed<AgentTaskPhase>(() => task.value?.phase ?? 'idle')
  const statusText = computed(() => task.value?.statusText || task.value?.lastError || '')

  /** 任务是否属于这本书（防误显示/误取消他书任务）。 */
  function taskForBook(bookId: string) {
    return task.value?.bookId === bookId && task.value.jobId ? task.value : null
  }

  async function pollOnce() {
    const requestedBookId = currentBookId.value
    if (!requestedBookId) {
      task.value = null
      return
    }
    try {
      const snapshot = await getAgentTaskStatus(requestedBookId)
      // 等待期间切了书：旧书的响应直接丢弃
      if (currentBookId.value !== requestedBookId) return
      task.value = snapshot
    } catch {
      // 网络抖动/切书后的失败：不把旧书的跳变状态续到新书上
      if (currentBookId.value !== requestedBookId) return
      if (task.value?.running) scheduleNextPoll()
      return
    }

    const current = task.value
    const running = current?.running ?? false
    if (wasRunning.value && !running) {
      // 任务刚好结束：刷新资料（错误也已由服务端写入 lastError / 地图兜底）。
      // 任务恰好被注册表驱逐（404 → null）时也按当前书刷新一次。
      wasRunning.value = running
      await refreshMemoryOf(current?.bookId ?? requestedBookId)
      return
    }
    wasRunning.value = running

    if (!running) return
    scheduleNextPoll()
  }

  async function refreshMemoryOf(bookId: string | undefined) {
    if (!bookId || memory.value?.bookId !== bookId) return
    try {
      memory.value = await getAiBookMemory(bookId)
    } catch {
      /* 保留旧资料 */
    }
  }

  function scheduleNextPoll() {
    if (pollTimer != null) clearTimeout(pollTimer)
    const interval =
      typeof document !== 'undefined' && document.hidden
        ? POLL_INTERVAL_HIDDEN_MS
        : POLL_INTERVAL_MS
    pollTimer = setTimeout(() => {
      pollTimer = null
      void pollOnce()
    }, interval)
  }

  /** 开始轮询（幂等）。onMounted 与任务提交后都会调用。 */
  function startPolling() {
    if (polling) {
      // 上一个任务已结束时 pollOnce 不会再排定时器：这里补一次即时刷新，
      // 否则重开资料页不会恢复最新任务状态。
      if (pollTimer == null) void pollOnce()
      return
    }
    polling = true
    void pollOnce()
  }

  function stopPolling() {
    polling = false
    if (pollTimer != null) {
      clearTimeout(pollTimer)
      pollTimer = null
    }
  }

  function handleVisibilityChange() {
    if (polling && task.value?.running) scheduleNextPoll()
  }

  async function startTask(
    book: Book,
    kind: 'update-to-current' | 'redraw-map',
    targetChapterIndex?: number
  ) {
    if (!book.bookId) throw new Error('书籍缺少 bookId')
    currentBookId.value = book.bookId
    // 409（已有 AI 任务进行中）直接上抛：调用方需要 toast 真实原因。
    // 页面级 `maybeAutoUpdate` 会自己吞掉错误，不影响翻章自动更新。
    await runAgentTask({ bookId: book.bookId, kind, targetChapterIndex })
    wasRunning.value = false // 强制下一次 poll 走「刚启动」分支
    startPolling()
    await pollOnce()
    // 秒完成的任务（如整章跳过）不会经历 running→false 跳变：此刻已结束就直接刷新
    const current = task.value
    if (current && !current.running && current.bookId === book.bookId) {
      await refreshMemoryOf(book.bookId)
    }
  }

  function startUpdateToCurrent(book: Book, targetChapterIndex?: number) {
    return startTask(book, 'update-to-current', targetChapterIndex)
  }

  /// 翻章自动触发：保留「开关开启 + 进度未到」的前置门槛（与旧版一致）；
  /// 失败静默接受——lastError 留在服务端注册表，下次打开页面可见。
  async function maybeAutoUpdate(book: Book, completedChapterIndex: number) {
    try {
      if (!book.bookId) return
      // 复用 load() 回写 memory.value：null 结果会写成 emptyMemory(book)，
      // 没开 AI 的书下一次翻章直接命中本地记忆，不再逐章询问 getAiBookMemory；
      // 请求失败时 load() 抛错、由外层 catch 静默吞掉，memory 保持原状（与原行为一致）
      const current =
        memory.value?.bookId === book.bookId ? memory.value : await load(book)
      if (!current?.enabled) return
      if ((current.processedChapterIndex ?? -1) >= completedChapterIndex) return
      await startTask(book, 'update-to-current', completedChapterIndex)
    } catch {
      /* 静默 */
    }
  }

  function startRedrawMap(book: Book) {
    return startTask(book, 'redraw-map')
  }

  async function cancelCurrentTask() {
    // 只取消当前查看/轮询的书：task 可能还是上一本书的旧快照，
    // 不能拿它当取消目标（服务端书级 DELETE 只作用于路径里的 bookId）。
    const bookId = currentBookId.value
    if (!bookId) return
    await cancelAgentTask(bookId)
    await pollOnce()
  }

  return {
    memory,
    loading,
    task,
    isBusy,
    phase,
    statusText,
    serverModelStatus,
    canUseServerModel: computed(() => Boolean(serverModelStatus.value?.canUseServerModel)),
    serverTextReady: computed(() => Boolean(serverModelStatus.value?.textReady)),
    serverImageReady: computed(() => Boolean(serverModelStatus.value?.imageReady)),
    serverSpeechReady: computed(() => Boolean(serverModelStatus.value?.speechReady)),
    agentReady: computed(() => Boolean(serverModelStatus.value?.agentReady)),
    loadServerModelStatus,
    load,
    save,
    setEnabled,
    reset,
    taskForBook,
    startPolling,
    stopPolling,
    handleVisibilityChange,
    startUpdateToCurrent,
    startRedrawMap,
    maybeAutoUpdate,
    cancelCurrentTask,
  }
})

/** 视图在 load/reset 后始终持有可渲染的记忆骨架（与旧 createEmptyAiBookMemory 一致）。 */
function emptyMemory(book: Book): AiBookMemory {
  return {
    bookId: book.bookId,
    bookUrl: book.bookUrl,
    bookName: book.name,
    author: book.author,
    enabled: false,
    updatedAt: Date.now(),
    summary: '',
    worldview: [],
    characters: [],
    relationships: [],
    locations: [],
    map: null,
    mapDirty: false,
  }
}
