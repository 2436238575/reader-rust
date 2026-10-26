// @vitest-environment jsdom
import { computed, ref } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import {
  CONTINUOUS_PRUNE_KEEP_ABOVE,
  CONTINUOUS_PRUNE_KEEP_BELOW,
  shouldPruneLower,
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
    // 正文管线（fetchChapterContent）在 readerContent store：mock 对象同形，直接复用
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

describe('shouldPruneLower 下部裁剪选择逻辑', () => {
  it('keeps the active chapter and its neighbours within the buffer', () => {
    expect(shouldPruneLower(5, 5)).toBe(false)
    expect(shouldPruneLower(6, 5)).toBe(false)
    // 边界：恰好 activeIndex + keepBelow 不裁
    expect(shouldPruneLower(5 + CONTINUOUS_PRUNE_KEEP_BELOW, 5)).toBe(false)
  })

  it('prunes chapters beyond the buffer', () => {
    expect(shouldPruneLower(5 + CONTINUOUS_PRUNE_KEEP_BELOW + 1, 5)).toBe(true)
    expect(shouldPruneLower(100, 5)).toBe(true)
  })

  it('never selects chapters above the active one（那是上部裁剪的活）', () => {
    expect(shouldPruneLower(0, 5)).toBe(false)
    expect(shouldPruneLower(4, 5)).toBe(false)
  })

  it('honours a custom keepBelow', () => {
    expect(shouldPruneLower(6, 5, 1)).toBe(false)
    expect(shouldPruneLower(7, 5, 1)).toBe(true)
  })
})

describe('useContinuousReading 下部裁剪', () => {
  async function loadEight(reading: ReturnType<typeof makeReading>) {
    await reading.initializeContinuousChapters(0)
    for (let index = 1; index <= 7; index += 1) {
      await reading.loadContinuousNext()
    }
  }

  it('prunes chapters below the keep buffer into equal-height placeholders', async () => {
    const store = makeStore(10)
    const reading = makeReading(store)
    await loadEight(reading)
    expect(reading.continuousChapters.value).toHaveLength(8)

    reading.setContinuousActiveChapter(2, 'x', 0)

    const chapters = reading.continuousChapters.value
    for (const chapter of chapters) {
      if (shouldPruneLower(chapter.index, 2)) {
        expect(chapter.prunedHeight).toBeGreaterThan(0)
        expect(chapter.content).toBe('')
        expect(chapter.html).toBe('')
      } else {
        // 当前章与下方缓冲内章节保持完整
        expect(chapter.prunedHeight).toBeUndefined()
        expect(chapter.html).not.toBe('')
      }
    }
    // 占位不删条目：章节总数与顺序不变
    expect(chapters.map((c) => c.index)).toEqual([0, 1, 2, 3, 4, 5, 6, 7])
  })

  it('does not re-process chapters already pruned（占位高度保持原值）', async () => {
    const store = makeStore(10)
    const reading = makeReading(store)
    await loadEight(reading)
    reading.setContinuousActiveChapter(2, 'x', 0)
    expect(reading.getContinuousChapter(7)?.prunedHeight).toBeGreaterThan(0)

    // 手动改成可识别的高度，再跑一轮裁剪：已裁剪章节不应被重写
    reading.continuousChapters.value = reading.continuousChapters.value.map((chapter) =>
      chapter.index === 7 ? { ...chapter, prunedHeight: 999 } : chapter
    )
    reading.pruneLowerChapters(2)
    expect(reading.getContinuousChapter(7)?.prunedHeight).toBe(999)
  })

  it('rehydrates a lower-pruned chapter with real content when scrolling back down', async () => {
    const store = makeStore(10)
    const reading = makeReading(store)
    await loadEight(reading)
    reading.setContinuousActiveChapter(2, 'x', 0)
    expect(reading.getContinuousChapter(7)?.prunedHeight).toBeGreaterThan(0)

    await reading.rehydrateContinuousChapter(7)

    const chapter = reading.getContinuousChapter(7)
    expect(chapter?.prunedHeight).toBeUndefined()
    expect(chapter?.content).toBe('第7章正文内容')
    expect(chapter?.html).toContain('第7章正文内容')
  })

  it('never prunes below in hide-read mode（上下滚动2 有自己的整章移除）', async () => {
    const store = makeStore(10)
    const reading = makeReading(store, true)
    await loadEight(reading)
    reading.setContinuousActiveChapter(2, 'x', 0)
    expect(reading.continuousChapters.value.every((c) => c.prunedHeight == null)).toBe(true)
  })
})
