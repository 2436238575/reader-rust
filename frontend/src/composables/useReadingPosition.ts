import { nextTick, ref } from 'vue'
import type { ComputedRef, Ref } from 'vue'
import type { useReaderStore } from '../stores/reader'
import type { ContinuousChapterItem } from './useContinuousReading'
import { safeLocalSet } from '../utils/storage'
import {
  buildServerSavedPosition,
  getPositionStorageKey,
  normalizePositionTimestamp,
  type SavedReadingPosition,
} from '../utils/readingPosition'

type ReaderStore = ReturnType<typeof useReaderStore>

/// 连续滚动模式判定活动章节的锚线位置（视口顶部往下 12%）
export const CONTINUOUS_POSITION_ANCHOR_RATIO = 0.12

// 排查阅读位置恢复问题时临时打开
const POSITION_DEBUG = false

function debugPositionLog(message: string, payload?: unknown) {
  if (!POSITION_DEBUG) return
  console.debug(`[reader-position] ${message}`, payload ?? '')
}

export interface ReadingPositionOptions {
  store: ReaderStore
  scrollContainerRef: Ref<HTMLElement | undefined>
  chapterTextRef: Ref<HTMLElement | undefined>
  isContinuousMode: ComputedRef<boolean>
  isHorizontalPageMode: ComputedRef<boolean>
  iosWebkit: boolean
  /// 横翻分支：页列表、页码（可写）、瞬切标记与末页状态刷新
  horizontalPages: Ref<string[]>
  horizontalPageIndex: Ref<number>
  suppressHorizontalTransitionOnce: () => void
  updateHorizontalEndState: () => void
  /// 连续滚动分支：按章节目定位
  continuousChapters: Ref<ContinuousChapterItem[]>
}

/**
 * 阅读位置的保存与恢复。
 *
 * 存档 = { chapterIndex, progress, paragraphIndex?, paragraphProgress?, updatedAt }，
 * localStorage 为主、服务端进度（book.durChapter*）参与新旧比较（毫秒归一后取新）。
 * 恢复走「pending + 防抖重试（最多 12 次）」：内容/页数未就绪时等下一帧再试，
 * 恢复成功后开 400-2500ms 的保存抑制窗，防恢复中的 scroll 事件立刻回写覆盖。
 */
export function useReadingPosition(options: ReadingPositionOptions) {
  const {
    store,
    scrollContainerRef,
    chapterTextRef,
    isContinuousMode,
    isHorizontalPageMode,
    iosWebkit,
    horizontalPages,
    horizontalPageIndex,
    suppressHorizontalTransitionOnce,
    updateHorizontalEndState,
    continuousChapters,
  } = options

  const pendingRestorePosition = ref<SavedReadingPosition | null>(null)
  let pendingRestoreAttempts = 0
  let restorePositionTimer: number | null = null
  let persistPositionTimer: number | null = null
  const restoreStabilizeTimers: number[] = []
  // 恢复成功后短暂禁止回写：恢复引发的 scroll 事件不能把刚恢复的位置又存回去
  let suppressPositionSaveUntil = 0
  // 连续滚动的同步/自动加载抑制：恢复/跳章重建后给视口稳定留出窗口
  let suppressContinuousScrollSyncUntil = 0
  let suppressContinuousAutoLoadUntil = 0

  /** 外部写入待恢复位置（切章/书签跳转/模式切换），并重置重试计数 */
  function queueRestorePosition(position: SavedReadingPosition) {
    pendingRestorePosition.value = position
    pendingRestoreAttempts = 0
  }

  function loadSavedReadingPosition() {
    const key = getPositionStorageKey(store.book?.bookId)
    if (!key) {
      pendingRestorePosition.value = null
      pendingRestoreAttempts = 0
      debugPositionLog('skip load: no storage key')
      return
    }
    try {
      const raw = localStorage.getItem(key)
      const localSaved = raw ? (JSON.parse(raw) as SavedReadingPosition) : null
      const serverSaved = buildServerSavedPosition(
        store.book,
        store.currentIndex,
        store.serverProgressTime
      )

      let selected: SavedReadingPosition | null = null
      let source: 'local' | 'server' | 'none' = 'none'

      if (localSaved && localSaved.chapterIndex === store.currentIndex) {
        selected = localSaved
        source = 'local'
      }

      if (serverSaved && serverSaved.chapterIndex === store.currentIndex) {
        if (
          !selected ||
          normalizePositionTimestamp(serverSaved.updatedAt) >
            normalizePositionTimestamp(selected.updatedAt)
        ) {
          selected = serverSaved
          source = 'server'
        }
      }

      if (!selected) {
        pendingRestorePosition.value = null
        pendingRestoreAttempts = 0
        clearRestoreStabilizers()
        debugPositionLog(raw ? 'ignored saved position' : 'no saved position', {
          key,
          currentIndex: store.currentIndex,
          localSaved,
          serverSaved,
        })
        return
      }

      queueRestorePosition(selected)
      clearRestoreStabilizers()
      debugPositionLog('loaded saved position', {
        key,
        saved: selected,
        source,
        localSaved,
        serverSaved,
        currentIndex: store.currentIndex,
      })
      // 刚加载出待恢复位置的 2.5s 内不回写：等恢复落地再开始存
      suppressPositionSaveUntil = Date.now() + 2500
    } catch {
      pendingRestorePosition.value = null
      pendingRestoreAttempts = 0
      clearRestoreStabilizers()
      debugPositionLog('failed to parse saved position', { key })
    }
  }

  function saveReadingPosition(options: { force?: boolean } = {}) {
    const key = getPositionStorageKey(store.book?.bookId)
    const container = scrollContainerRef.value
    const suppressed = !options.force && Date.now() < suppressPositionSaveUntil
    if (!key || !container || store.loading || !store.book || suppressed) {
      debugPositionLog('skip save', {
        key,
        hasContainer: !!container,
        loading: store.loading,
        hasBook: !!store.book,
        suppressed,
        currentIndex: store.currentIndex,
      })
      return
    }

    const basePosition: SavedReadingPosition = {
      chapterIndex: store.currentIndex,
      progress: store.chapterScrollProgress,
      updatedAt: Date.now(),
    }

    const anchorRatio = isContinuousMode.value ? CONTINUOUS_POSITION_ANCHOR_RATIO : 0.3
    const anchorViewportY =
      container.getBoundingClientRect().top + container.clientHeight * anchorRatio
    if (isContinuousMode.value && continuousChapters.value.length) {
      const section = container.querySelector(
        `.continuous-chapter[data-chapter-index="${store.currentIndex}"]`
      ) as HTMLElement | null
      const paragraphs = Array.from(
        section?.querySelectorAll('.chapter-text p') || []
      ) as HTMLElement[]
      if (paragraphs.length) {
        const { index, progress } = locateAnchorParagraph(paragraphs, anchorViewportY)
        basePosition.paragraphIndex = index
        basePosition.paragraphProgress = progress
      }
    } else if (!isHorizontalPageMode.value) {
      const paragraphs = Array.from(
        chapterTextRef.value?.querySelectorAll('p') || []
      ) as HTMLElement[]
      if (paragraphs.length) {
        const { index, progress } = locateAnchorParagraph(paragraphs, anchorViewportY)
        basePosition.paragraphIndex = index
        basePosition.paragraphProgress = progress
      }
    }

    safeLocalSet(key, JSON.stringify(basePosition))
    debugPositionLog('saved position', { key, position: basePosition })
  }

  function scheduleSaveReadingPosition() {
    if (persistPositionTimer) clearTimeout(persistPositionTimer)
    persistPositionTimer = window.setTimeout(() => {
      persistPositionTimer = null
      saveReadingPosition()
    }, 120)
  }

  function restoreReadingPosition() {
    return restoreReadingPositionInternal(pendingRestorePosition.value, true)
  }

  function clearRestoreStabilizers() {
    while (restoreStabilizeTimers.length) {
      const timer = restoreStabilizeTimers.pop()
      if (typeof timer === 'number') clearTimeout(timer)
    }
  }

  // iOS 竖滚：布局在恢复后还会抖几次，140/320/680ms 三次补偿恢复
  function scheduleRestoreStabilization(saved: SavedReadingPosition) {
    clearRestoreStabilizers()
    if (!iosWebkit || isHorizontalPageMode.value) return
    ;[140, 320, 680].forEach((delay) => {
      const timer = window.setTimeout(() => {
        if (store.loading || !scrollContainerRef.value || saved.chapterIndex !== store.currentIndex)
          return
        void nextTick(() => {
          restoreReadingPositionInternal(saved, false)
        })
      }, delay)
      restoreStabilizeTimers.push(timer)
    })
  }

  function restoreReadingPositionInternal(
    saved: SavedReadingPosition | null,
    finalize: boolean
  ): boolean {
    const container = scrollContainerRef.value
    if (!saved || !container || saved.chapterIndex !== store.currentIndex) {
      debugPositionLog('restore aborted', {
        hasSaved: !!saved,
        hasContainer: !!container,
        savedChapterIndex: saved?.chapterIndex,
        currentIndex: store.currentIndex,
      })
      return false
    }

    if (isHorizontalPageMode.value) {
      // 横翻由 transform 分页驱动：写 scrollLeft 会被对齐/翻页逻辑立刻归零（此前的
      // 恢复因此是死代码，横翻离开再回来永远在第 1 页）。恢复 = 按进度落到第几页
      const maxPage = Math.max(0, horizontalPages.value.length - 1)
      if (store.loading || maxPage <= 0) {
        debugPositionLog('restore waiting: horizontal pages not ready', {
          saved,
          loading: store.loading,
          pageCount: horizontalPages.value.length,
        })
        return false
      }
      suppressHorizontalTransitionOnce()
      horizontalPageIndex.value = Math.max(
        0,
        Math.min(maxPage, Math.round(maxPage * Math.max(0, Math.min(1, saved.progress || 0))))
      )
      updateHorizontalEndState()
      if (finalize) {
        pendingRestorePosition.value = null
        pendingRestoreAttempts = 0
      }
      debugPositionLog('restored horizontal position', {
        saved,
        pageIndex: horizontalPageIndex.value,
      })
      return true
    }

    const anchorOffset =
      container.clientHeight * (isContinuousMode.value ? CONTINUOUS_POSITION_ANCHOR_RATIO : 0.3)
    let targetTop = 0

    if (isContinuousMode.value) {
      if (store.loading || !continuousChapters.value.length) {
        debugPositionLog('restore waiting: continuous content not ready', {
          saved,
          loading: store.loading,
          continuousCount: continuousChapters.value.length,
        })
        return false
      }
      const section = container.querySelector(
        `.continuous-chapter[data-chapter-index="${saved.chapterIndex}"]`
      ) as HTMLElement | null
      if (!section) {
        debugPositionLog('restore failed: section not found', {
          saved,
          availableSections: Array.from(container.querySelectorAll('.continuous-chapter')).map(
            (el) => (el as HTMLElement).dataset.chapterIndex
          ),
        })
        return false
      }
      const paragraphs = Array.from(section.querySelectorAll('.chapter-text p')) as HTMLElement[]
      if (typeof saved.paragraphIndex === 'number' && !paragraphs.length) {
        debugPositionLog('restore waiting: continuous paragraphs not ready', {
          saved,
          sectionIndex: saved.chapterIndex,
        })
        return false
      }
      if (paragraphs.length && typeof saved.paragraphIndex === 'number') {
        const paragraph =
          paragraphs[Math.max(0, Math.min(paragraphs.length - 1, saved.paragraphIndex))]
        const top =
          paragraph.getBoundingClientRect().top -
          container.getBoundingClientRect().top +
          container.scrollTop
        const paragraphProgress = Math.max(0, Math.min(1, saved.paragraphProgress || 0))
        targetTop = Math.max(
          section.offsetTop,
          top + paragraph.offsetHeight * paragraphProgress - anchorOffset
        )
      } else {
        const nextSection = section.nextElementSibling as HTMLElement | null
        const sectionHeight = Math.max(
          1,
          (nextSection ? nextSection.offsetTop : container.scrollHeight) - section.offsetTop
        )
        if (
          (saved.progress || 0) > 0 &&
          sectionHeight <= Math.max(1, container.clientHeight * 0.25)
        ) {
          debugPositionLog('restore waiting: continuous section height not ready', {
            saved,
            sectionHeight,
            clientHeight: container.clientHeight,
          })
          return false
        }
        targetTop = Math.max(
          section.offsetTop,
          section.offsetTop + sectionHeight * Math.max(0, Math.min(1, saved.progress || 0))
        )
      }
    } else {
      const paragraphs = Array.from(
        chapterTextRef.value?.querySelectorAll('p') || []
      ) as HTMLElement[]
      if (store.loading || !chapterTextRef.value) {
        debugPositionLog('restore waiting: chapter content not ready', {
          saved,
          loading: store.loading,
          hasChapterText: !!chapterTextRef.value,
        })
        return false
      }
      if (typeof saved.paragraphIndex === 'number' && !paragraphs.length) {
        debugPositionLog('restore waiting: chapter paragraphs not ready', {
          saved,
        })
        return false
      }
      if (paragraphs.length && typeof saved.paragraphIndex === 'number') {
        const paragraph =
          paragraphs[Math.max(0, Math.min(paragraphs.length - 1, saved.paragraphIndex))]
        const top =
          paragraph.getBoundingClientRect().top -
          container.getBoundingClientRect().top +
          container.scrollTop
        const paragraphProgress = Math.max(0, Math.min(1, saved.paragraphProgress || 0))
        targetTop = top + paragraph.offsetHeight * paragraphProgress - anchorOffset
      } else {
        const maxScroll = Math.max(0, container.scrollHeight - container.clientHeight)
        if ((saved.progress || 0) > 0 && maxScroll <= 4) {
          debugPositionLog('restore waiting: max scroll not ready', {
            saved,
            scrollHeight: container.scrollHeight,
            clientHeight: container.clientHeight,
            maxScroll,
          })
          return false
        }
        targetTop = maxScroll * Math.max(0, Math.min(1, saved.progress || 0))
      }
    }

    // 恢复定位要瞬时完成：'auto' 会跟随容器 CSS 的 scroll-behavior:smooth，
    // 恢复一个几千像素的位置会爬行数秒，看起来像卡在旧位置
    container.scrollTo({ top: Math.max(0, targetTop), behavior: 'instant' as ScrollBehavior })
    if (finalize) {
      pendingRestorePosition.value = null
      pendingRestoreAttempts = 0
      const suppressMs = isContinuousMode.value && iosWebkit ? 1600 : 500
      suppressContinuousScrollSyncUntil = Date.now() + suppressMs
      suppressContinuousAutoLoadUntil = Date.now() + suppressMs
      scheduleRestoreStabilization(saved)
    }
    suppressPositionSaveUntil = Date.now() + 400
    debugPositionLog('restored vertical position', {
      saved,
      targetTop,
      isContinuous: isContinuousMode.value,
      finalize,
    })
    return true
  }

  function scheduleRestoreReadingPosition() {
    if (restorePositionTimer) clearTimeout(restorePositionTimer)
    debugPositionLog('schedule restore', {
      attempts: pendingRestoreAttempts,
      hasPending: !!pendingRestorePosition.value,
      currentIndex: store.currentIndex,
    })
    restorePositionTimer = window.setTimeout(
      () => {
        restorePositionTimer = null
        void nextTick(() => {
          const restored = restoreReadingPosition()
          if (!restored && pendingRestorePosition.value && pendingRestoreAttempts < 12) {
            pendingRestoreAttempts += 1
            debugPositionLog('restore retry', {
              attempts: pendingRestoreAttempts,
              pending: pendingRestorePosition.value,
              currentIndex: store.currentIndex,
            })
            scheduleRestoreReadingPosition()
          } else if (!restored) {
            debugPositionLog('restore gave up', {
              attempts: pendingRestoreAttempts,
              pending: pendingRestorePosition.value,
              currentIndex: store.currentIndex,
            })
            pendingRestorePosition.value = null
            pendingRestoreAttempts = 0
          }
        })
      },
      pendingRestoreAttempts === 0 ? 0 : 80
    )
  }

  /** 连续滚动重建目录跳章时：给视口稳定留 500ms 窗口（读侧在 handleScroll） */
  function suppressContinuousInteractions(ms = 500) {
    suppressContinuousScrollSyncUntil = Date.now() + ms
    suppressContinuousAutoLoadUntil = Date.now() + ms
  }

  function isContinuousScrollSyncSuppressed() {
    return Date.now() < suppressContinuousScrollSyncUntil
  }

  function isContinuousAutoLoadSuppressed() {
    return Date.now() < suppressContinuousAutoLoadUntil
  }

  function disposeReadingPosition() {
    if (restorePositionTimer) clearTimeout(restorePositionTimer)
    if (persistPositionTimer) clearTimeout(persistPositionTimer)
    clearRestoreStabilizers()
  }

  return {
    pendingRestorePosition,
    queueRestorePosition,
    loadSavedReadingPosition,
    saveReadingPosition,
    scheduleSaveReadingPosition,
    restoreReadingPosition,
    scheduleRestoreReadingPosition,
    suppressContinuousInteractions,
    isContinuousScrollSyncSuppressed,
    isContinuousAutoLoadSuppressed,
    disposeReadingPosition,
  }
}

/// 段落锚点：视口锚线掠过的最后一个段落 + 段内进度
function locateAnchorParagraph(paragraphs: HTMLElement[], anchorViewportY: number) {
  let activeParagraph = paragraphs[0]
  let paragraphIndex = 0
  paragraphs.forEach((paragraph, index) => {
    if (paragraph.getBoundingClientRect().top <= anchorViewportY) {
      activeParagraph = paragraph
      paragraphIndex = index
    }
  })
  const rect = activeParagraph.getBoundingClientRect()
  const paragraphProgress =
    rect.height > 0 ? Math.max(0, Math.min(1, (anchorViewportY - rect.top) / rect.height)) : 0
  return { index: paragraphIndex, progress: paragraphProgress }
}
