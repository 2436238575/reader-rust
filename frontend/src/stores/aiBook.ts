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
 * - 快照带 bookUrl——A 书任务跑着、打开 B 书页面时按钮不受影响。
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
      const saved = await getAiBookMemory(book.bookUrl)
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
    const current = memory.value?.bookUrl === book.bookUrl ? memory.value : await load(book)
    if (!current) return null
    return save({
      ...current,
      bookUrl: book.bookUrl,
      bookName: book.name,
      author: book.author,
      enabled,
      updatedAt: Date.now(),
    })
  }

  async function reset(book: Book) {
    await deleteAiBookMemory(book.bookUrl)
    memory.value = emptyMemory(book)
    return memory.value
  }

  // -- 任务状态与轮询 --------------------------------------------------------

  const task = ref<AgentTaskSnapshot | null>(null)
  const wasRunning = ref(false)
  let pollTimer: ReturnType<typeof setTimeout> | null = null
  let polling = false

  const isBusy = computed(() => Boolean(task.value?.running))
  const phase = computed<AgentTaskPhase>(() => task.value?.phase ?? 'idle')
  const statusText = computed(() => task.value?.statusText || task.value?.lastError || '')

  /** 任务是否属于这本书（防误显示/误取消他书任务）。 */
  function taskForBook(bookUrl: string) {
    return task.value?.bookUrl === bookUrl && task.value.jobId ? task.value : null
  }

  async function pollOnce() {
    let snapshot: AgentTaskSnapshot | null = null
    try {
      snapshot = await getAgentTaskStatus()
      task.value = snapshot
    } catch {
      // 网络抖动：保持旧快照，按隐藏态间隔重试
    }

    const current = task.value
    const running = current?.running ?? false
    if (wasRunning.value && !running) {
      // 任务刚好结束：刷新资料（错误也已由服务端写入 lastError / 地图兜底）
      wasRunning.value = running
      await refreshMemoryOf(current?.bookUrl)
      return
    }
    wasRunning.value = running

    if (!running) return
    scheduleNextPoll()
  }

  async function refreshMemoryOf(bookUrl: string | undefined) {
    if (!bookUrl || memory.value?.bookUrl !== bookUrl) return
    try {
      memory.value = await getAiBookMemory(bookUrl)
    } catch {
      /* 保留旧资料 */
    }
  }

  function scheduleNextPoll() {
    if (pollTimer != null) clearTimeout(pollTimer)
    const interval =
      typeof document !== 'undefined' && document.hidden ? POLL_INTERVAL_HIDDEN_MS : POLL_INTERVAL_MS
    pollTimer = setTimeout(() => {
      pollTimer = null
      void pollOnce()
    }, interval)
  }

  /** 开始轮询（幂等）。onMounted 与任务提交后都会调用。 */
  function startPolling() {
    if (polling) return
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
    kind: 'update_to_current' | 'redraw_map',
    targetChapterIndex?: number
  ) {
    try {
      await runAgentTask({ bookUrl: book.bookUrl, kind, targetChapterIndex })
    } catch (error) {
      // 409：已有任务进行中（可能是别的书）——切到轮询展示即可
      if (!isConflict(error)) throw error
    }
    wasRunning.value = false // 强制下一次 poll 走「刚启动」分支
    startPolling()
    await pollOnce()
    // 秒完成的任务（如整章跳过）不会经历 running→false 跳变：此刻已结束就直接刷新
    const current = task.value
    if (current && !current.running && current.bookUrl === book.bookUrl) {
      await refreshMemoryOf(book.bookUrl)
    }
  }

  function startUpdateToCurrent(book: Book, targetChapterIndex?: number) {
    return startTask(book, 'update_to_current', targetChapterIndex)
  }

  /// 翻章自动触发：保留「开关开启 + 进度未到」的前置门槛（与旧版一致）；
  /// 失败静默接受——lastError 留在服务端注册表，下次打开页面可见。
  async function maybeAutoUpdate(book: Book, completedChapterIndex: number) {
    try {
      const current =
        memory.value?.bookUrl === book.bookUrl
          ? memory.value
          : await getAiBookMemory(book.bookUrl).catch(() => null)
      if (!current?.enabled) return
      if ((current.processedChapterIndex ?? -1) >= completedChapterIndex) return
      await startTask(book, 'update_to_current', completedChapterIndex)
    } catch {
      /* 静默 */
    }
  }

  function startRedrawMap(book: Book) {
    return startTask(book, 'redraw_map')
  }

  async function cancelCurrentTask() {
    await cancelAgentTask()
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

function isConflict(error: unknown) {
  return Boolean(
    error &&
      typeof error === 'object' &&
      'status' in error &&
      (error as { status?: number }).status === 409
  )
}
