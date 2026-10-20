import { computed, onMounted, onUnmounted, ref } from 'vue'
import type { useReaderStore } from '../stores/reader'

type ReaderStore = ReturnType<typeof useReaderStore>

export interface ReaderTtsControlsOptions {
  /// 段落朗读推进（useReaderAutoPlayback 提供）
  startSpeech: () => void
  cancelSpeechTransition: () => void
  restartSpeechFromCurrentParagraph: () => void
}

/// TTS 面板的粘合逻辑：面板开关/关闭即忽略、播放暂停切换、语速音调定时器
/// 调整。调整类操作同构：写 store → 面板保持打开 → 朗读中则就地重起。
export function useReaderTtsControls(store: ReaderStore, options: ReaderTtsControlsOptions) {
  const showTTSPanel = ref(false)
  const ttsPanelDismissed = ref(false)

  const speechTimerNow = ref(Date.now())
  let speechTimerTicker: number | null = null

  const speechTimerText = computed(() => {
    if (!store.speechStopAt) return ''
    const remainMs = store.speechStopAt - speechTimerNow.value
    if (remainMs <= 0) return ''
    const totalMinutes = Math.ceil(remainMs / 60000)
    if (totalMinutes >= 60) {
      const hours = Math.floor(totalMinutes / 60)
      const minutes = totalMinutes % 60
      return minutes ? `${hours}小时${minutes}分钟后停止` : `${hours}小时后停止`
    }
    return `${totalMinutes}分钟后停止`
  })

  onMounted(() => {
    speechTimerTicker = window.setInterval(() => {
      speechTimerNow.value = Date.now()
    }, 15000)
  })
  onUnmounted(() => {
    if (speechTimerTicker) clearInterval(speechTimerTicker)
  })

  function handleTTS() {
    // 朗读中/暂停中点工具栏按钮＝暂停/继续（按钮 title 就是这么承诺的），
    // 同时打开面板；未开始时才只是打开面板
    if (store.isSpeaking || store.isPaused) {
      toggleSpeechFromPanel()
      return
    }
    ttsPanelDismissed.value = false
    showTTSPanel.value = true
  }

  function closeTTSPanel() {
    showTTSPanel.value = false
    ttsPanelDismissed.value = true
  }

  function toggleSpeechFromPanel() {
    ttsPanelDismissed.value = false
    showTTSPanel.value = true
    // 暂停态恢复优先（pauseTTS 是 toggle）：此前 isSpeaking=false 会走成从头重读
    if (store.isPaused) {
      store.pauseTTS()
      return
    }
    if (!store.isSpeaking) {
      options.startSpeech()
      return
    }
    options.cancelSpeechTransition()
    store.pauseTTS()
  }

  function handleStopTTS() {
    options.cancelSpeechTransition()
    store.stopTTS()
  }

  /// 调整类操作的公共形态：面板保持打开 + 朗读中就地重起
  function keepPanelAndRestart() {
    ttsPanelDismissed.value = false
    showTTSPanel.value = true
    if (store.isSpeaking && !store.isPaused) {
      options.restartSpeechFromCurrentParagraph()
    }
  }

  function changeVoice(name: string) {
    store.setVoiceName(name)
    keepPanelAndRestart()
  }

  function changeOpenAIVoice(voiceId: string) {
    if (store.speechConfig.openaiSource === 'server') return
    store.setOpenAISpeechVoice(voiceId)
    keepPanelAndRestart()
  }

  function adjustSpeechRate(delta: number) {
    const next = Math.max(
      0.5,
      Math.min(3, parseFloat((store.speechConfig.speechRate + delta).toFixed(1)))
    )
    store.setSpeechRate(next)
    keepPanelAndRestart()
  }

  function adjustSpeechPitch(delta: number) {
    const next = Math.max(
      0.5,
      Math.min(2, parseFloat((store.speechConfig.speechPitch + delta).toFixed(1)))
    )
    store.setSpeechPitch(next)
    keepPanelAndRestart()
  }

  function setSpeechTimer(minutes: number) {
    store.setSpeechStopTimer(minutes)
    // 定时停止不重起朗读（与其他调整类操作不同——它不改变发声参数）
    ttsPanelDismissed.value = false
    showTTSPanel.value = true
  }

  return {
    showTTSPanel,
    ttsPanelDismissed,
    speechTimerText,
    handleTTS,
    closeTTSPanel,
    toggleSpeechFromPanel,
    handleStopTTS,
    changeVoice,
    changeOpenAIVoice,
    adjustSpeechRate,
    adjustSpeechPitch,
    setSpeechTimer,
  }
}
