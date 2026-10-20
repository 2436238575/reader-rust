// @vitest-environment jsdom
import { computed, ref } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import {
  CONTINUOUS_PRUNE_KEEP_ABOVE,
  useContinuousReading,
} from './useContinuousReading'

function makeStore(chapterCount: number) {
  const chapters = Array.from({ length: chapterCount }, (_, index) => ({
    title: `第${index + 1}章`,
    url: `local-txt:test#${index}`,
  }))
  return {
    chapters,
    currentIndex: 0,
    content: '',
    loading: false,
    isChapterRead: vi.fn(() => false),
    setActiveChapterState: vi.fn(),
    markChapterAsRead: vi.fn(),
    persistProgress: vi.fn(async () => {}),
    fetchChapterContent: vi.fn(async (index: number) => `第${index}章正文内容`),
  }
}

function makeContainer() {
  // jsdom 里 offsetHeight 恒为 0：占位高度走 || 1 兜底，逻辑链路与高度值无关
  const container = document.createElement('div')
  return ref<HTMLElement | undefined>(container)
}

function makeReading(store: ReturnType<typeof makeStore>, hideRead = false) {
  return useContinuousReading(
    store as never,
    (raw: string) => `<p>${raw}</p>`,
    computed(() => true),
    computed(() => hideRead),
    makeContainer()
  )
}

describe('useContinuousReading 上部裁剪', () => {
  it('prunes chapters above the keep buffer into equal-height placeholders', async () => {
    const store = makeStore(10)
    const reading = makeReading(store)
    await reading.initializeContinuousChapters(0)
    // 手动把 0..7 章都塞进来（模拟一路向下加载）
    for (let index = 1; index <= 7; index += 1) {
      await reading.loadContinuousNext()
    }
    expect(reading.continuousChapters.value).toHaveLength(8)

    reading.setContinuousActiveChapter(7, 'x', 0)

    const chapters = reading.continuousChapters.value
    const cutoff = 7 - CONTINUOUS_PRUNE_KEEP_ABOVE
    for (const chapter of chapters) {
      if (chapter.index < cutoff) {
        expect(chapter.prunedHeight).toBeGreaterThan(0)
        expect(chapter.content).toBe('')
        expect(chapter.html).toBe('')
      } else {
        expect(chapter.prunedHeight).toBeUndefined()
        expect(chapter.html).not.toBe('')
      }
    }
    // 占位不删条目：章节总数与顺序不变
    expect(chapters.map((c) => c.index)).toEqual([0, 1, 2, 3, 4, 5, 6, 7])
  })

  it('rehydrates a pruned chapter with real content', async () => {
    const store = makeStore(10)
    const reading = makeReading(store)
    await reading.initializeContinuousChapters(0)
    for (let index = 1; index <= 7; index += 1) {
      await reading.loadContinuousNext()
    }
    reading.setContinuousActiveChapter(7, 'x', 0)
    expect(reading.getContinuousChapter(0)?.prunedHeight).toBeGreaterThan(0)

    await reading.rehydrateContinuousChapter(0)

    const chapter = reading.getContinuousChapter(0)
    expect(chapter?.prunedHeight).toBeUndefined()
    expect(chapter?.content).toBe('第0章正文内容')
    expect(chapter?.html).toContain('第0章正文内容')
  })

  it('never prunes in hide-read mode（上下滚动2 有自己的整章移除）', async () => {
    const store = makeStore(10)
    const reading = makeReading(store, true)
    await reading.initializeContinuousChapters(0)
    for (let index = 1; index <= 7; index += 1) {
      await reading.loadContinuousNext()
    }
    reading.setContinuousActiveChapter(7, 'x', 0)
    expect(reading.continuousChapters.value.every((c) => c.prunedHeight == null)).toBe(true)
  })

  it('does not re-render placeholders when syncing html（排版配置变化时）', async () => {
    const store = makeStore(10)
    const reading = makeReading(store)
    await reading.initializeContinuousChapters(0)
    for (let index = 1; index <= 7; index += 1) {
      await reading.loadContinuousNext()
    }
    reading.setContinuousActiveChapter(7, 'x', 0)

    reading.syncContinuousChapterHtml()

    const placeholder = reading.getContinuousChapter(0)
    expect(placeholder?.prunedHeight).toBeGreaterThan(0)
    expect(placeholder?.html).toBe('')
  })

  it('placeholder keeps its slot so progress anchoring still finds the section', async () => {
    const store = makeStore(10)
    const reading = makeReading(store)
    await reading.initializeContinuousChapters(0)
    for (let index = 1; index <= 7; index += 1) {
      await reading.loadContinuousNext()
    }
    reading.setContinuousActiveChapter(7, 'x', 0)
    // getContinuousChapter 对占位章仍返回条目（滚动锚定靠 index 对齐）
    expect(reading.getContinuousChapter(1)?.title).toBe('第2章')
  })
})
