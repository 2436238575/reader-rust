import { nextTick, ref } from 'vue'
import type { ComputedRef, Ref } from 'vue'
import type { useReaderStore } from '../stores/reader'

type ReaderStore = ReturnType<typeof useReaderStore>

export interface ContinuousChapterItem {
  index: number
  title: string
  content: string
  html: string
  /** 上部裁剪占位高度（px）：被裁章节清空 content/html，占位保持滚动高度不跳动 */
  prunedHeight?: number
}

/// 当前阅读位置上方保留的完整章节数；更早的章节卸载为占位。
/// 3 章缓冲：往上回翻一两章不用等网络，再往上才触发回载
export const CONTINUOUS_PRUNE_KEEP_ABOVE = 3

export function useContinuousReading(
  store: ReaderStore,
  renderChapterHtml: (rawText: string, chapterIndex?: number) => string,
  isContinuousMode: ComputedRef<boolean>,
  hideReadChaptersMode: ComputedRef<boolean>,
  scrollContainerRef: Ref<HTMLElement | undefined>
) {
  const continuousChapters = ref<ContinuousChapterItem[]>([])
  const continuousLoadingNext = ref(false)
  const suppressContinuousSync = ref(false)
  let continuousStateSyncTimer: number | null = null
  const rehydratingIndexes = new Set<number>()

  function shouldHideChapter(index: number, keepIndex?: number) {
    if (!hideReadChaptersMode.value) return false
    if (typeof keepIndex === 'number' && index === keepIndex) return false
    return store.isChapterRead(index)
  }

  function findNextVisibleIndex(startIndex: number, keepIndex?: number) {
    for (let index = startIndex; index < store.chapters.length; index += 1) {
      if (!shouldHideChapter(index, keepIndex)) {
        return index
      }
    }
    return -1
  }

  function pruneReadChapters(targetIndex = store.currentIndex) {
    if (!hideReadChaptersMode.value) return
    continuousChapters.value = continuousChapters.value.filter(
      (chapter) => chapter.index >= targetIndex
    )
  }

  /// 默认「上下滚动」模式的上部裁剪：当前章上方超过缓冲数的章节卸载为
  /// 等高占位（清空 content/html 双份字符串——长阅读时这是内存增长的大头）。
  /// 「上下滚动2」走 pruneReadChapters 整章移除，不经过这里。
  function pruneUpperChapters(activeIndex: number) {
    if (hideReadChaptersMode.value) return
    const cutoff = activeIndex - CONTINUOUS_PRUNE_KEEP_ABOVE
    if (cutoff <= 0) return
    // 每次滚动都会走到这里：没有可裁的章节时连数组都不重建，避免滚动路径上分配
    if (!continuousChapters.value.some((c) => c.index < cutoff && c.prunedHeight == null)) return
    const container = scrollContainerRef.value
    continuousChapters.value = continuousChapters.value.map((chapter) => {
      if (chapter.index >= cutoff || chapter.prunedHeight != null) return chapter
      const section = container?.querySelector(
        `.continuous-chapter[data-chapter-index="${chapter.index}"]`
      ) as HTMLElement | null
      return {
        ...chapter,
        content: '',
        html: '',
        prunedHeight: section?.offsetHeight || 1,
      }
    })
  }

  /// 回载被裁剪的章节：拿到真实内容后换回占位，并修正滚动位置
  /// （占位高度与实际渲染高度的差值会推挤下方内容）
  async function rehydrateContinuousChapter(index: number) {
    if (rehydratingIndexes.has(index)) return
    const chapter = getContinuousChapter(index)
    if (!chapter || chapter.prunedHeight == null) return
    rehydratingIndexes.add(index)
    try {
      const rebuilt = await buildContinuousChapter(index)
      if (!rebuilt) return
      const container = scrollContainerRef.value
      const section = container?.querySelector(
        `.continuous-chapter[data-chapter-index="${index}"]`
      ) as HTMLElement | null
      const placeholderHeight = chapter.prunedHeight
      const wasAboveViewport =
        container && section ? section.offsetTop + placeholderHeight <= container.scrollTop : false
      continuousChapters.value = continuousChapters.value.map((item) =>
        item.index === index ? rebuilt : item
      )
      await nextTick()
      // 回载章节在视口上方时，新高度会把当前阅读位置推跑：按高度差补偿 scrollTop
      if (container && section && wasAboveViewport) {
        const delta = section.offsetHeight - placeholderHeight
        if (delta !== 0) container.scrollTop += delta
      }
    } finally {
      rehydratingIndexes.delete(index)
    }
  }

  async function buildContinuousChapter(index: number, forceRefresh = false) {
    const chapter = store.chapters[index]
    if (!chapter) return null
    const chapterContent = await store.fetchChapterContent(index, forceRefresh)
    if (chapterContent == null) return null
    return {
      index,
      title: chapter.title,
      content: chapterContent,
      html: renderChapterHtml(chapterContent, index),
    } satisfies ContinuousChapterItem
  }

  function syncContinuousChapterHtml() {
    continuousChapters.value = continuousChapters.value.map((chapter) => {
      // 占位章没有内容可重渲；保留 prunedHeight 等回载时换新
      if (chapter.prunedHeight != null) return chapter
      return {
        ...chapter,
        html: renderChapterHtml(chapter.content, chapter.index),
      }
    })
  }

  function getContinuousChapter(index: number) {
    return continuousChapters.value.find((chapter) => chapter.index === index) || null
  }

  function setContinuousActiveChapter(index: number, chapterContent: string, progress: number) {
    suppressContinuousSync.value = true
    store.setActiveChapterState(index, chapterContent, progress)
    store.markChapterAsRead(index)
    void store.persistProgress(index)
    pruneUpperChapters(index)
    if (continuousStateSyncTimer) {
      clearTimeout(continuousStateSyncTimer)
    }
    continuousStateSyncTimer = window.setTimeout(() => {
      suppressContinuousSync.value = false
    }, 0)
  }

  async function initializeContinuousChapters(targetIndex = store.currentIndex, smooth = false) {
    if (!isContinuousMode.value || !store.chapters[targetIndex]) return

    const current = await buildContinuousChapter(targetIndex)
    if (!current) return

    continuousChapters.value = [current]
    setContinuousActiveChapter(targetIndex, current.content, 0)

    await nextTick()
    scrollToContinuousChapter(targetIndex, smooth)

    const nextIndex = hideReadChaptersMode.value
      ? findNextVisibleIndex(targetIndex + 1, targetIndex)
      : targetIndex + 1
    if (nextIndex < 0) return

    void (async () => {
      const next = await buildContinuousChapter(nextIndex)
      if (!next) return
      if (continuousChapters.value.some((chapter) => chapter.index === next.index)) return
      continuousChapters.value = [...continuousChapters.value, next]
    })()
  }

  async function syncContinuousToStoreState() {
    if (
      !isContinuousMode.value ||
      suppressContinuousSync.value ||
      store.loading ||
      !store.chapters[store.currentIndex]
    )
      return

    pruneReadChapters(store.currentIndex)
    const current = getContinuousChapter(store.currentIndex)
    if (current) {
      if (current.prunedHeight != null) {
        // 当前章被裁成占位（模式切换等边角路径）：回载而不是同步空内容
        void rehydrateContinuousChapter(current.index)
        return
      }
      if (current.content !== store.content) {
        current.content = store.content
        current.html = renderChapterHtml(store.content, current.index)
      }
      return
    }

    await initializeContinuousChapters(store.currentIndex, false)
  }

  async function loadContinuousNext() {
    if (continuousLoadingNext.value || !continuousChapters.value.length) return
    const last = continuousChapters.value[continuousChapters.value.length - 1]
    const nextIndex = hideReadChaptersMode.value
      ? findNextVisibleIndex(last.index + 1, store.currentIndex)
      : last.index + 1
    if (nextIndex >= store.chapters.length) return

    continuousLoadingNext.value = true
    try {
      const next = await buildContinuousChapter(nextIndex)
      if (next && !getContinuousChapter(next.index)) {
        continuousChapters.value = [...continuousChapters.value, next]
      }
    } finally {
      continuousLoadingNext.value = false
    }
  }

  function getContinuousSections() {
    const container = scrollContainerRef.value
    if (!container) return [] as HTMLElement[]
    return Array.from(container.querySelectorAll('.continuous-chapter')) as HTMLElement[]
  }

  function scrollToContinuousChapter(index: number, smooth = true) {
    const container = scrollContainerRef.value
    if (!container) return
    const target = getContinuousChapter(index)
    if (target && target.prunedHeight != null) {
      // 目录点到被裁剪的章节：先回载再跳，否则跳过去的是空白占位
      void rehydrateContinuousChapter(index).then(() => scrollToContinuousChapter(index, smooth))
      return
    }
    const section = container.querySelector(
      `.continuous-chapter[data-chapter-index="${index}"]`
    ) as HTMLElement | null
    if (!section) return
    container.scrollTo({
      top: Math.max(0, section.offsetTop),
      behavior: smooth ? 'smooth' : 'auto',
    })
  }

  function clearContinuousChapters() {
    continuousChapters.value = []
  }

  function disposeContinuousReading() {
    if (continuousStateSyncTimer) {
      clearTimeout(continuousStateSyncTimer)
      continuousStateSyncTimer = null
    }
  }

  return {
    continuousChapters,
    continuousLoadingNext,
    suppressContinuousSync,
    syncContinuousChapterHtml,
    getContinuousChapter,
    setContinuousActiveChapter,
    initializeContinuousChapters,
    syncContinuousToStoreState,
    loadContinuousNext,
    getContinuousSections,
    pruneReadChapters,
    pruneUpperChapters,
    rehydrateContinuousChapter,
    clearContinuousChapters,
    disposeContinuousReading,
  }
}
