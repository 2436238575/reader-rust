// @vitest-environment jsdom
import { createApp, reactive } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import { useReaderTtsControls } from './useReaderTtsControls'

function makeStore(overrides: Record<string, unknown> = {}) {
  return reactive({
    isSpeaking: false,
    isPaused: false,
    speechStopAt: 0,
    speechConfig: { speechRate: 1.0, speechPitch: 1.0, openaiSource: 'local' },
    pauseTTS: vi.fn(),
    stopTTS: vi.fn(),
    setVoiceName: vi.fn(),
    setOpenAISpeechVoice: vi.fn(),
    setSpeechRate: vi.fn(),
    setSpeechPitch: vi.fn(),
    setSpeechStopTimer: vi.fn(),
    ...overrides,
  })
}

/// composable 里用了 onMounted/onUnmounted（计时器），需要宿主组件
function withSetup<T>(composable: () => T) {
  let result!: T
  const app = createApp({
    setup() {
      result = composable()
      return () => null
    },
  })
  app.mount(document.createElement('div'))
  return { result, unmount: () => app.unmount() }
}

describe('useReaderTtsControls', () => {
  it('未朗读时 handleTTS 只开面板；朗读中等于暂停/继续切换', () => {
    const store = makeStore()
    const startSpeech = vi.fn()
    const { result } = withSetup(() =>
      useReaderTtsControls(store as never, {
        startSpeech,
        cancelSpeechTransition: vi.fn(),
        restartSpeechFromCurrentParagraph: vi.fn(),
      })
    )
    result.handleTTS()
    expect(result.showTTSPanel.value).toBe(true)
    expect(startSpeech).not.toHaveBeenCalled()

    // 朗读中再点：暂停（pauseTTS 是 toggle）
    store.isSpeaking = true
    result.handleTTS()
    expect(store.pauseTTS).toHaveBeenCalledTimes(1)
  })

  it('暂停态恢复优先：toggleSpeechFromPanel 走 pauseTTS 而不是从头重读', () => {
    const store = makeStore({ isPaused: true })
    const startSpeech = vi.fn()
    const { result } = withSetup(() =>
      useReaderTtsControls(store as never, {
        startSpeech,
        cancelSpeechTransition: vi.fn(),
        restartSpeechFromCurrentParagraph: vi.fn(),
      })
    )
    result.toggleSpeechFromPanel()
    expect(store.pauseTTS).toHaveBeenCalledTimes(1)
    expect(startSpeech).not.toHaveBeenCalled()
  })

  it('语速调整钳在 0.5-3，朗读中就地重起', () => {
    const store = makeStore({ isSpeaking: true })
    const restart = vi.fn()
    const { result } = withSetup(() =>
      useReaderTtsControls(store as never, {
        startSpeech: vi.fn(),
        cancelSpeechTransition: vi.fn(),
        restartSpeechFromCurrentParagraph: restart,
      })
    )
    store.speechConfig.speechRate = 3.0
    result.adjustSpeechRate(0.5)
    expect(store.setSpeechRate).toHaveBeenCalledWith(3.0) // 超上限被钳
    expect(restart).toHaveBeenCalledTimes(1)
    // 暂停中不重起
    store.isPaused = true
    result.adjustSpeechRate(-0.5)
    expect(store.setSpeechRate).toHaveBeenCalledWith(2.5)
    expect(restart).toHaveBeenCalledTimes(1)
  })

  it('定时停止文本：不足一小时报分钟，超出报小时+分钟', () => {
    const store = makeStore()
    const { result } = withSetup(() =>
      useReaderTtsControls(store as never, {
        startSpeech: vi.fn(),
        cancelSpeechTransition: vi.fn(),
        restartSpeechFromCurrentParagraph: vi.fn(),
      })
    )
    expect(result.speechTimerText.value).toBe('')
    // speechTimerNow 在 composable 初始化时取钟，比这里早几毫秒——
    // 用 ceil 桶内的安全值（44.5 分钟 → 45）避开边界抖动
    store.speechStopAt = Date.now() + 44.5 * 60 * 1000
    expect(result.speechTimerText.value).toBe('45分钟后停止')
    store.speechStopAt = Date.now() + 89.5 * 60 * 1000
    expect(result.speechTimerText.value).toBe('1小时30分钟后停止')
    store.speechStopAt = Date.now() - 1000
    expect(result.speechTimerText.value).toBe('')
  })

  it('服务端音源时不允许换 OpenAI 音色', () => {
    const store = makeStore()
    store.speechConfig.openaiSource = 'server'
    const { result } = withSetup(() =>
      useReaderTtsControls(store as never, {
        startSpeech: vi.fn(),
        cancelSpeechTransition: vi.fn(),
        restartSpeechFromCurrentParagraph: vi.fn(),
      })
    )
    result.changeOpenAIVoice('alloy')
    expect(store.setOpenAISpeechVoice).not.toHaveBeenCalled()
  })
})
