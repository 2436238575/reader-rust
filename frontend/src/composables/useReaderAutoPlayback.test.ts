import { computed, ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useReaderAutoPlayback } from './useReaderAutoPlayback'

describe('useReaderAutoPlayback', () => {
  it('starts speech when chapter HTML contains br elements without paragraphs', () => {
    const chapterText = fakeElement('第一段\n第二段', [], '第一段<br>第二段')
    const scrollContainer = fakeElement('')
    const originalHtml = chapterText.innerHTML
    const startTTS = vi.fn()
    const store = {
      speechConfig: {
        provider: 'openai',
        openaiRequestMode: 'chunked',
      },
      currentIndex: 0,
      hasNext: false,
      hasPrev: false,
      isPaused: false,
      isAutoScrolling: false,
      startTTS,
      preloadOpenAITTS: vi.fn(),
      stopTTS: vi.fn(),
    } as unknown as Parameters<typeof useReaderAutoPlayback>[0]

    const playback = useReaderAutoPlayback(
      store,
      computed(() => ({
        autoPageMode: 'pixel',
        clickAction: 'none',
        scrollPixel: 1,
        pageSpeed: 1_000,
        fontSize: 16,
        lineHeight: 1.5,
      })),
      computed(() => false),
      ref(scrollContainer),
      ref(chapterText),
      vi.fn(),
      vi.fn()
    )

    playback.startSpeech()

    expect(startTTS).toHaveBeenCalledTimes(1)
    expect(startTTS.mock.calls[0]?.[0]).toContain('第一段')
    expect(chapterText.innerHTML).toBe(originalHtml)
  })

  it('keeps paragraph elements as separate speech targets when they exist', () => {
    const firstParagraph = fakeElement('第一段')
    const secondParagraph = fakeElement('第二段')
    const chapterText = fakeElement('第一段\n第二段', [firstParagraph, secondParagraph])
    const scrollContainer = fakeElement('')
    const startTTS = vi.fn()
    const store = {
      speechConfig: {
        provider: 'system',
        openaiRequestMode: 'chunked',
      },
      currentIndex: 0,
      hasNext: false,
      hasPrev: false,
      isPaused: false,
      isAutoScrolling: false,
      startTTS,
      preloadOpenAITTS: vi.fn(),
      stopTTS: vi.fn(),
    } as unknown as Parameters<typeof useReaderAutoPlayback>[0]

    const playback = useReaderAutoPlayback(
      store,
      computed(() => ({
        autoPageMode: 'pixel',
        clickAction: 'none',
        scrollPixel: 1,
        pageSpeed: 1_000,
        fontSize: 16,
        lineHeight: 1.5,
      })),
      computed(() => false),
      ref(scrollContainer),
      ref(chapterText),
      vi.fn(),
      vi.fn()
    )

    playback.startSpeech()

    expect(startTTS).toHaveBeenCalledTimes(1)
    expect(startTTS.mock.calls[0]?.[0]).toBe('第一段')
  })
})

describe('pixel auto-scroll', () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })
  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
  })

  function makeScrollable() {
    // 可滚动的容器假元素：scrollTop/clientHeight/scrollHeight 可自由读写
    return {
      scrollTop: 0,
      clientHeight: 700,
      scrollHeight: 2100,
      innerText: '',
      offsetTop: 0,
      offsetHeight: 700,
      isConnected: true,
      classList: { add: vi.fn(), remove: vi.fn() },
      querySelector: vi.fn(() => null),
      querySelectorAll: vi.fn(() => []),
      scrollTo: vi.fn(),
    } as unknown as HTMLElement
  }

  function makeStore() {
    return {
      speechConfig: { provider: 'system', openaiRequestMode: 'chunked' },
      currentIndex: 0,
      hasNext: true,
      hasPrev: false,
      isPaused: false,
      isSpeaking: false,
      isAutoScrolling: false,
      startTTS: vi.fn(),
      preloadOpenAITTS: vi.fn(),
      stopTTS: vi.fn(),
    } as unknown as Parameters<typeof useReaderAutoPlayback>[0]
  }

  function pumpFrames(queue: FrameRequestCallback[], n: number) {
    for (let i = 0; i < n; i++) {
      const cb = queue.shift()
      if (!cb) return
      cb(performance.now())
    }
  }

  it('到底翻章后由 handleContentChanged 接力恢复滚动', async () => {
    const rafQueue: FrameRequestCallback[] = []
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
      rafQueue.push(cb)
      return rafQueue.length
    })
    vi.stubGlobal('cancelAnimationFrame', vi.fn())

    const container = makeScrollable()
    const store = makeStore()
    const nextChapter = vi.fn(async () => {
      // 模拟章节切换：内容变短（新章），索引前进
      store.currentIndex = 1
      container.scrollTop = 0
    })
    const playback = useReaderAutoPlayback(
      store,
      computed(() => ({
        autoPageMode: 'pixel',
        clickAction: 'none',
        scrollPixel: 100,
        pageSpeed: 1000,
        fontSize: 16,
        lineHeight: 1.5,
      })),
      computed(() => false),
      ref(container),
      ref(undefined),
      nextChapter,
      vi.fn()
    )

    store.isAutoScrolling = true
    playback.startAutoScroll()
    // 推进到章末（scrollHeight 2100 - clientHeight 700 = 1400，speed=50/帧）
    pumpFrames(rafQueue, 40)
    expect(nextChapter).toHaveBeenCalledTimes(1)
    // 翻章 Promise 落地前 rAF 链是断的
    await vi.advanceTimersByTimeAsync(0)
    // 新章正文到达：接力重启
    playback.handleContentChanged()
    pumpFrames(rafQueue, 10)
    expect(container.scrollTop).toBeGreaterThan(0)
  })

  it('章末续翻不再绑在 clickAction 上', async () => {
    const rafQueue: FrameRequestCallback[] = []
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
      rafQueue.push(cb)
      return rafQueue.length
    })
    vi.stubGlobal('cancelAnimationFrame', vi.fn())

    const container = makeScrollable()
    const store = makeStore()
    const nextChapter = vi.fn(async () => {
      store.currentIndex = 1
    })
    const playback = useReaderAutoPlayback(
      store,
      // clickAction: 'none' 也要在章末续翻
      computed(() => ({
        autoPageMode: 'pixel',
        clickAction: 'none',
        scrollPixel: 100,
        pageSpeed: 1000,
        fontSize: 16,
        lineHeight: 1.5,
      })),
      computed(() => false),
      ref(container),
      ref(undefined),
      nextChapter,
      vi.fn()
    )

    store.isAutoScrolling = true
    playback.startAutoScroll()
    pumpFrames(rafQueue, 40)
    expect(nextChapter).toHaveBeenCalledTimes(1)
  })

  it('翻章失败（索引未变）时停住自动滚动', async () => {
    const rafQueue: FrameRequestCallback[] = []
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
      rafQueue.push(cb)
      return rafQueue.length
    })
    vi.stubGlobal('cancelAnimationFrame', vi.fn())

    const container = makeScrollable()
    const store = makeStore()
    const nextChapter = vi.fn(async () => {
      /* 失败：索引不动 */
    })
    const playback = useReaderAutoPlayback(
      store,
      computed(() => ({
        autoPageMode: 'pixel',
        clickAction: 'auto',
        scrollPixel: 100,
        pageSpeed: 1000,
        fontSize: 16,
        lineHeight: 1.5,
      })),
      computed(() => false),
      ref(container),
      ref(undefined),
      nextChapter,
      vi.fn()
    )

    store.isAutoScrolling = true
    playback.startAutoScroll()
    pumpFrames(rafQueue, 40)
    expect(nextChapter).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(0)
    expect(store.isAutoScrolling).toBe(false)
  })
})

function fakeElement(innerText: string, paragraphs: HTMLElement[] = [], innerHTML = '') {
  return {
    innerText,
    innerHTML,
    offsetTop: 0,
    offsetHeight: 20,
    scrollTop: 0,
    isConnected: true,
    classList: {
      add: vi.fn(),
      remove: vi.fn(),
    },
    querySelector: vi.fn(() => null),
    querySelectorAll: vi.fn((selector: string) => (selector === 'p' ? paragraphs : [])),
    scrollTo: vi.fn(),
    // paragraphSpeakText 会 clone 后剔除评论气泡再取文本；假元素没有气泡，原样返回文本
    cloneNode: vi.fn(() => ({
      querySelectorAll: vi.fn(() => []),
      textContent: innerText,
    })),
  } as unknown as HTMLElement
}
