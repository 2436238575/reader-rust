import { ref, computed, onUnmounted } from 'vue'
import { getAvailableBookSourceSSE } from '../api/search'
import { useAppStore } from '../stores/app'
import { useSourceStore } from '../stores/source'
import type { SearchBook } from '../types'

/** 换源搜索的「当前书」口径：候选按 name/author 匹配、按 latest 对齐进度 */
export interface AvailableSourceCurrent {
  bookId?: string
  name: string
  author?: string
  /** 当前绑定的书源 id（候选里同源的直接排除） */
  sourceId?: number
  latest?: string
}

export interface AvailableSourceCandidate {
  book: SearchBook
  sameName: boolean
  sameAuthor: boolean
  sameLatest: boolean
  chapterHint: string
  score: number
}

type AvailableSourceMode = 'initial' | 'loadMore'

type AvailableSourceSSEPayload = {
  data?: SearchBook[]
  books?: SearchBook[]
  lastIndex?: number
  hasMore?: boolean
}

const AVAILABLE_CONCURRENT_COUNT = 8

export function normalizeSourceText(value?: string) {
  return (value || '')
    .replace(/\s+/g, '')
    .replace(/[：:,.，。！？!?\-—_()（）【】\[\]<>《》'"“”‘’]/g, '')
    .toLowerCase()
}

export function normalizeSourceAuthorText(value?: string) {
  return normalizeSourceText(value).replace(/^作者/, '')
}

/**
 * 「bookSourceId → 书源名」的翻译器：后端搜索结果不带源名，用书源库补可读名。
 * 书源库可能还没拉过（书架页直接开详情弹窗），调用方自行保证 fetchSources。
 */
export function useSourceNameResolver() {
  const sourceStore = useSourceStore()
  return (bookSourceId?: number) =>
    sourceStore.sources.find((source) => source.bookSourceId === bookSourceId)?.bookSourceName ||
    ''
}

/**
 * 全网搜同名书（换源候选）的唯一实现：SSE 分批搜索、同源去重、
 * 作者不一致的候选直接丢弃，按书名/作者/最新章节打分排序。
 * 阅读器换源面板与详情弹窗换源共用，避免各写一份后行为漂移。
 */
export function useAvailableSources(getCurrent: () => AvailableSourceCurrent | null) {
  const appStore = useAppStore()
  const searching = ref(false)
  const loadingMore = ref(false)
  const results = ref<SearchBook[]>([])
  const lastIndex = ref(-1)
  const hasMoreSources = ref(true)
  let stream: EventSource | null = null

  const preparedResults = computed<AvailableSourceCandidate[]>(() => {
    const current = getCurrent()
    if (!current) return []
    const currentName = normalizeSourceText(current.name)
    const currentAuthor = normalizeSourceAuthorText(current.author)
    const currentLatest = normalizeSourceText(current.latest)

    return results.value
      .map((book) => {
        const sameName = normalizeSourceText(book.name) === currentName
        const sameAuthor = normalizeSourceAuthorText(book.author) === currentAuthor
        const sameLatest =
          !!currentLatest && normalizeSourceText(book.lastChapter) === currentLatest
        const chapterHint = sameLatest
          ? '可无缝续读'
          : book.lastChapter
            ? `目标源最新：${book.lastChapter}`
            : ''
        const score = (sameName ? 3 : 0) + (sameAuthor ? 3 : 0) + (sameLatest ? 4 : 0)
        return { book, sameName, sameAuthor, sameLatest, chapterHint, score }
      })
      .sort((a, b) => b.score - a.score)
  })

  function closeStream() {
    if (!stream) return
    stream.close()
    stream = null
  }

  onUnmounted(closeStream)

  function parsePayload(event: MessageEvent): AvailableSourceSSEPayload | null {
    try {
      return JSON.parse(event.data) as AvailableSourceSSEPayload
    } catch (error) {
      console.error('parse getAvailableBookSourceSSE payload failed', error)
      return null
    }
  }

  function mergeCandidates(candidates: SearchBook[]) {
    const current = getCurrent()
    if (!current || !candidates.length) return
    const currentAuthor = normalizeSourceAuthorText(current.author)
    candidates.forEach((item) => {
      // 后端已排除当前源；这里再挡一次陈旧缓存/竞态
      if (current.sourceId != null && item.bookSourceId === current.sourceId) return
      if (currentAuthor && item.author && normalizeSourceAuthorText(item.author) !== currentAuthor)
        return
      // 同书源即重复
      const existed = results.value.some(
        (candidate) => candidate.bookSourceId === item.bookSourceId
      )
      if (!existed) {
        results.value.push(item)
      }
    })
  }

  function applyPayload(payload: AvailableSourceSSEPayload | null) {
    if (!payload) return
    const incoming = Array.isArray(payload.data)
      ? payload.data
      : Array.isArray(payload.books)
        ? payload.books
        : []

    if (typeof payload.lastIndex === 'number') {
      lastIndex.value = payload.lastIndex
    }
    if (typeof payload.hasMore === 'boolean') {
      hasMoreSources.value = payload.hasMore
    }
    mergeCandidates(incoming)
  }

  function openStream(mode: AvailableSourceMode) {
    const current = getCurrent()
    if (!current?.bookId) return

    const beforeCount = results.value.length
    const sse = getAvailableBookSourceSSE({
      bookId: current.bookId,
      lastIndex: mode === 'loadMore' ? lastIndex.value : -1,
      concurrentCount: AVAILABLE_CONCURRENT_COUNT,
    })
    stream = sse

    sse.onmessage = (event) => {
      if (stream !== sse) return
      applyPayload(parsePayload(event))
    }

    sse.addEventListener('end', (event) => {
      if (stream !== sse) return
      applyPayload(parsePayload(event as MessageEvent))
      finishStream(sse, mode, beforeCount)
    })

    sse.onerror = (event) => {
      if (stream !== sse) return
      console.error('getAvailableBookSourceSSE failed', event)
      finishStream(sse, mode, beforeCount, true)
    }
  }

  function finishStream(
    sse: EventSource,
    mode: AvailableSourceMode,
    beforeCount: number,
    failed = false
  ) {
    if (stream !== sse) return
    sse.close()
    stream = null

    if (mode === 'initial') {
      searching.value = false
    } else {
      loadingMore.value = false
    }

    if (failed) {
      if (mode === 'loadMore') {
        appStore.showToast('加载更多书源失败', 'error')
      }
      return
    }

    if (mode === 'loadMore') {
      const addedCount = results.value.length - beforeCount
      if (addedCount > 0) {
        appStore.showToast(`已新增 ${addedCount} 个书源`, 'success')
      } else if (!hasMoreSources.value) {
        appStore.showToast('没有更多书源了', 'warning')
      } else {
        appStore.showToast('本批次未找到更多匹配书源', 'warning')
      }
    }
  }

  function startSearch() {
    if (!getCurrent()) return
    closeStream()
    searching.value = true
    loadingMore.value = false
    results.value = []
    lastIndex.value = -1
    hasMoreSources.value = true

    openStream('initial')
  }

  function loadMoreSources() {
    if (!getCurrent() || loadingMore.value || !hasMoreSources.value) return

    closeStream()
    loadingMore.value = true
    openStream('loadMore')
  }

  return {
    searching,
    loadingMore,
    hasMoreSources,
    preparedResults,
    startSearch,
    loadMoreSources,
  }
}
