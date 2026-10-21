// @vitest-environment jsdom
import { computed, ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useReadingPosition } from './useReadingPosition'
import { READER_POSITION_PREFIX } from '../utils/readingPosition'

function makeContainer() {
  const container = document.createElement('div')
  document.body.appendChild(container)
  return container
}

function makeHarness(overrides: {
  book?: Record<string, unknown> | null
  currentIndex?: number
  horizontal?: boolean
  continuous?: boolean
  loading?: boolean
}) {
  const store = {
    book: overrides.book ?? { bookId: 'book_1', durChapterIndex: 0, durChapterPos: 0 },
    currentIndex: overrides.currentIndex ?? 0,
    chapterScrollProgress: 0,
    loading: overrides.loading ?? false,
    serverProgressTime: 0,
  }
  const container = makeContainer()
  const scrollContainerRef = ref<HTMLElement | undefined>(container)
  const chapterTextRef = ref<HTMLElement | undefined>(document.createElement('div'))
  const horizontalPages = ref<string[]>(['<p>p1</p>', '<p>p2</p>', '<p>p3</p>'])
  const horizontalPageIndex = ref(0)
  const continuousChapters = ref([{ index: 0, title: 't', content: 'c', html: '<p>c</p>' }])
  const pos = useReadingPosition({
    store: store as never,
    scrollContainerRef,
    chapterTextRef,
    isContinuousMode: computed(() => overrides.continuous ?? false),
    isHorizontalPageMode: computed(() => overrides.horizontal ?? false),
    iosWebkit: false,
    horizontalPages,
    horizontalPageIndex,
    suppressHorizontalTransitionOnce: vi.fn(),
    updateHorizontalEndState: vi.fn(),
    continuousChapters: continuousChapters as never,
  })
  return { store, container, horizontalPageIndex, pos }
}

describe('useReadingPosition', () => {
  beforeEach(() => {
    localStorage.clear()
    vi.useFakeTimers()
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  it('loadSavedReadingPosition 选本地与服务端较新的存档', () => {
    const key = `${READER_POSITION_PREFIX}book_1`
    const NOW = 1_700_000_000_000
    // 本地更新（毫秒时间戳，比服务端新）
    localStorage.setItem(
      key,
      JSON.stringify({ chapterIndex: 0, progress: 0.3, updatedAt: NOW + 1000 })
    )
    const local = makeHarness({
      book: { bookId: 'book_1', durChapterIndex: 0, durChapterPos: 500, durChapterTime: NOW },
    })
    local.pos.loadSavedReadingPosition()
    expect(local.pos.pendingRestorePosition.value?.progress).toBe(0.3)

    // 服务端更新（durChapterTime 更大；pos 万分比 800 → 0.08）
    const server = makeHarness({
      book: { bookId: 'book_1', durChapterIndex: 0, durChapterPos: 800, durChapterTime: NOW + 9000 },
    })
    server.pos.loadSavedReadingPosition()
    expect(server.pos.pendingRestorePosition.value?.progress).toBe(0.08)

    // 存档是别的章的、且服务端进度也停在别的章：什么都不恢复
    localStorage.setItem(
      key,
      JSON.stringify({ chapterIndex: 9, progress: 0.9, updatedAt: NOW + 99_000 })
    )
    const other = makeHarness({
      book: { bookId: 'book_1', durChapterIndex: 2, durChapterPos: 0, durChapterTime: NOW },
    })
    other.pos.loadSavedReadingPosition()
    expect(other.pos.pendingRestorePosition.value).toBeNull()
  })

  it('加载待恢复位置后的 2.5s 抑制窗内不回写', () => {
    const key = `${READER_POSITION_PREFIX}book_1`
    localStorage.setItem(
      key,
      JSON.stringify({ chapterIndex: 0, progress: 0.5, updatedAt: 1_000 })
    )
    const { pos } = makeHarness({})
    pos.loadSavedReadingPosition()
    expect(pos.pendingRestorePosition.value).not.toBeNull()
    // 抑制窗内保存被吞
    pos.saveReadingPosition()
    const raw = localStorage.getItem(key)
    expect(raw).not.toBeNull()
    expect(JSON.parse(raw!).progress).toBe(0.5) // 没被 0 覆盖
  })

  it('横翻分支：按进度落到页码并清 pending', () => {
    const { pos, horizontalPageIndex } = makeHarness({ horizontal: true })
    pos.queueRestorePosition({ chapterIndex: 0, progress: 0.5, updatedAt: Date.now() })
    const ok = pos.restoreReadingPosition()
    expect(ok).toBe(true)
    // 3 页 → maxPage 2 → round(2*0.5)=1
    expect(horizontalPageIndex.value).toBe(1)
    expect(pos.pendingRestorePosition.value).toBeNull()
  })

  it('内容未就绪时重试，超过 12 次放弃并清 pending', async () => {
    // loading=true → 恢复永远失败
    const { pos } = makeHarness({ loading: true, horizontal: true })
    pos.queueRestorePosition({ chapterIndex: 0, progress: 0.5, updatedAt: Date.now() })
    pos.scheduleRestoreReadingPosition()
    // 首次 0ms 防抖 + 12 次 80ms 重试；async 版推进才会冲刷链里的 nextTick 微任务
    for (let i = 0; i < 14; i++) {
      await vi.advanceTimersByTimeAsync(100)
    }
    expect(pos.pendingRestorePosition.value).toBeNull()
  })

  it('竖滚保存-恢复往返：段落锚点写盘，恢复走 instant scrollTo', () => {
    const { pos, container, store } = makeHarness({})
    store.chapterScrollProgress = 0.42
    pos.saveReadingPosition({ force: true })
    const saved = JSON.parse(localStorage.getItem(`${READER_POSITION_PREFIX}book_1`)!)
    expect(saved.progress).toBe(0.42)

    // jsdom 没有布局：补 scrollHeight/clientHeight 才能越过「maxScroll 未就绪」守卫
    Object.defineProperty(container, 'scrollHeight', { value: 10000, configurable: true })
    Object.defineProperty(container, 'clientHeight', { value: 800, configurable: true })
    // jsdom 的 HTMLElement 没有 scrollTo 实现，直接挂 mock
    const scrollToMock = vi.fn()
    ;(container as unknown as { scrollTo: unknown }).scrollTo = scrollToMock
    pos.queueRestorePosition({ chapterIndex: 0, progress: 0.5, updatedAt: Date.now() })
    expect(pos.restoreReadingPosition()).toBe(true)
    expect(scrollToMock).toHaveBeenCalledWith(
      expect.objectContaining({ top: 4600, behavior: 'instant' })
    )
    expect(pos.pendingRestorePosition.value).toBeNull()
  })

  it('存档章与当前章不一致时恢复拒绝且不动容器', () => {
    const { pos, container } = makeHarness({ currentIndex: 2 })
    const scrollToMock = vi.fn()
    ;(container as unknown as { scrollTo: unknown }).scrollTo = scrollToMock
    pos.queueRestorePosition({ chapterIndex: 0, progress: 0.5, updatedAt: Date.now() })
    expect(pos.restoreReadingPosition()).toBe(false)
    expect(scrollToMock).not.toHaveBeenCalled()
  })
})
