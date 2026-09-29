import { computed, reactive, ref } from 'vue'
import type { Ref } from 'vue'
import { useAiBookStore } from '../stores/aiBook'
import { useAppStore } from '../stores/app'
import { safeLocalSet } from '../utils/storage'
import {
  DEFAULT_OPENAI_BASE_URL,
  requestOpenAISpeechAudio,
} from '../utils/openaiSpeech'

export interface TTSOptions {
  onStart?: () => void
  onEnd?: () => void
  onError?: (event?: SpeechSynthesisErrorEvent | Error) => void
}

interface PreloadedOpenAIAudio {
  key: string
  blob: Blob
}

const OPENAI_AUDIO_PRELOAD_LIMIT = 8

export type SpeechProvider = 'system' | 'openai'
export type OpenAISpeechSource = 'browser' | 'server'
export type OpenAISpeechFormat = 'mp3' | 'wav' | 'opus' | 'flac' | 'pcm'
export type OpenAISpeechRequestMode = 'chunked' | 'merged'

export interface SpeechConfig {
  provider: SpeechProvider
  voiceName: string
  speechRate: number
  speechPitch: number
  stopAfterMinutes: number
  openaiSource: OpenAISpeechSource
  openaiBaseUrl: string
  openaiApiKey: string
  openaiModel: string
  openaiVoice: string
  openaiFormat: OpenAISpeechFormat
  openaiRequestMode: OpenAISpeechRequestMode
}

const defaultSpeechConfig: SpeechConfig = {
  provider: 'system',
  voiceName: '',
  speechRate: 1,
  speechPitch: 1,
  stopAfterMinutes: 0,
  openaiSource: 'browser',
  openaiBaseUrl: DEFAULT_OPENAI_BASE_URL,
  openaiApiKey: '',
  openaiModel: 'qwen-tts',
  openaiVoice: 'vivian',
  openaiFormat: 'mp3',
  openaiRequestMode: 'chunked',
}

function loadSpeechConfig(): SpeechConfig {
  try {
    const saved = localStorage.getItem('reader-speechConfig')
    if (saved) return { ...defaultSpeechConfig, ...JSON.parse(saved) }
  } catch { /* ignore */ }
  return { ...defaultSpeechConfig }
}

function isSafariSpeechFallbackMode() {
  if (typeof navigator === 'undefined') return false
  const ua = navigator.userAgent || ''
  const vendor = navigator.vendor || ''
  const isAppleEngine = /Apple/i.test(vendor) || /iPhone|iPad|iPod/i.test(ua)
  return isAppleEngine && /Safari/i.test(ua) && !/Chrome|Chromium|CriOS|Edg|EdgiOS|Firefox|FxiOS|OPR|OPT|SamsungBrowser|Android/i.test(ua)
}

/**
 * 阅读器 TTS（系统语音 + OpenAI Speech）状态机。
 * 从 reader store 拆出：唯一的外部依赖是当前章正文（content ref），
 * 章节读完后的翻页推进由 useReaderAutoPlayback 负责。
 */
export function useReaderTts(content: Ref<string>) {
  const appStore = useAppStore()
  const aiBookStore = useAiBookStore()
  const isSpeaking = ref(false)
  const isSpeechLoading = ref(false)
  const isPaused = ref(false)
  const systemTtsNativeEventsReliable = ref(false)
  const voiceList = ref<SpeechSynthesisVoice[]>([])
  const speechConfig = reactive<SpeechConfig>(loadSpeechConfig())
  const openAISpeechConfigured = computed(() => {
    if (speechConfig.openaiSource === 'server') return true
    return !!speechConfig.openaiBaseUrl.trim()
  })
  const speechProviderLabel = computed(() => speechConfig.provider === 'openai' ? 'OpenAI Speech' : '系统语音')
  const speechStopAt = ref(0)
  let speechStopTimer: number | null = null
  let synth: SpeechSynthesis | null = typeof window !== 'undefined' ? window.speechSynthesis : null
  let currentUtterance: SpeechSynthesisUtterance | null = null
  let currentOpenAIAudio: HTMLAudioElement | null = null
  let currentOpenAIAudioUrl = ''
  let currentOpenAIAbortController: AbortController | null = null
  const preloadedOpenAIAudio = ref<PreloadedOpenAIAudio[]>([])
  let preloadGeneration = 0
  const inFlightPreloadKeys = new Set<string>()
  const inFlightOpenAIAudioRequests = new Map<string, Promise<Blob>>()
  let currentTTSSessionId = 0

  // 排查 TTS 状态机问题时临时打开
  const TTS_DEBUG = false

  function logTTS(message: string, payload?: unknown) {
    if (!TTS_DEBUG) return
    console.debug(`[TTS] ${message}`, payload ?? '')
  }

  function captureTTSCaller() {
    if (!TTS_DEBUG) return ''
    try {
      const stack = new Error().stack || ''
      return stack
        .split('\n')
        .slice(2, 5)
        .map((line) => line.trim())
        .join(' | ')
    } catch {
      return ''
    }
  }

  function beginTTSSession() {
    currentTTSSessionId += 1
    logTTS('begin session', { sessionId: currentTTSSessionId })
    return currentTTSSessionId
  }

  function isCurrentTTSSession(sessionId: number) {
    return sessionId === currentTTSSessionId
  }

  function saveSpeechConfig() {
    safeLocalSet('reader-speechConfig', JSON.stringify(speechConfig))
  }

  function fetchVoices() {
    if (!synth) return
    voiceList.value = synth.getVoices().slice().sort((a, b) => {
      const aZh = a.lang.startsWith('zh-')
      const bZh = b.lang.startsWith('zh-')
      if (aZh && !bZh) return -1
      if (!aZh && bZh) return 1
      return a.lang.localeCompare(b.lang)
    })
    if (!speechConfig.voiceName && voiceList.value.length > 0) {
      const zhVoice = voiceList.value.find((v) => v.lang.startsWith('zh-'))
      speechConfig.voiceName = (zhVoice || voiceList.value[0]).name
      saveSpeechConfig()
    }
  }

  function setVoiceName(name: string) {
    speechConfig.voiceName = name
    saveSpeechConfig()
  }

  function setSpeechProvider(provider: SpeechProvider) {
    speechConfig.provider = provider
    clearPreloadedOpenAIAudio()
    saveSpeechConfig()
  }

  function setOpenAISpeechBaseUrl(url: string) {
    speechConfig.openaiBaseUrl = url.trim()
    clearPreloadedOpenAIAudio()
    saveSpeechConfig()
  }

  function setOpenAISpeechSource(source: OpenAISpeechSource) {
    speechConfig.openaiSource = source
    clearPreloadedOpenAIAudio()
    saveSpeechConfig()
  }

  function setOpenAISpeechApiKey(apiKey: string) {
    speechConfig.openaiApiKey = apiKey.trim()
    clearPreloadedOpenAIAudio()
    saveSpeechConfig()
  }

  function setOpenAISpeechModel(model: string) {
    speechConfig.openaiModel = model
    clearPreloadedOpenAIAudio()
    saveSpeechConfig()
  }

  function setOpenAISpeechVoice(voice: string) {
    speechConfig.openaiVoice = voice
    clearPreloadedOpenAIAudio()
    saveSpeechConfig()
  }

  function setOpenAISpeechFormat(format: OpenAISpeechFormat) {
    speechConfig.openaiFormat = format
    clearPreloadedOpenAIAudio()
    saveSpeechConfig()
  }

  function setOpenAISpeechRequestMode(mode: OpenAISpeechRequestMode) {
    speechConfig.openaiRequestMode = mode
    clearPreloadedOpenAIAudio()
    saveSpeechConfig()
  }

  function setSpeechRate(rate: number) {
    speechConfig.speechRate = rate
    clearPreloadedOpenAIAudio()
    saveSpeechConfig()
  }

  function setSpeechPitch(pitch: number) {
    speechConfig.speechPitch = pitch
    saveSpeechConfig()
  }

  function buildOpenAIAudioCacheKey(rawText: string) {
    return [
      speechConfig.openaiSource,
      speechConfig.openaiBaseUrl.trim(),
      speechConfig.openaiApiKey.trim(),
      speechConfig.openaiModel,
      speechConfig.openaiVoice,
      speechConfig.openaiFormat,
      speechConfig.speechRate.toFixed(1),
      rawText,
    ].join('::')
  }

  async function fetchOpenAIAudioBlob(rawText: string, signal?: AbortSignal) {
    return requestOpenAISpeechAudio({
      source: speechConfig.openaiSource,
      baseUrl: speechConfig.openaiBaseUrl,
      apiKey: speechConfig.openaiApiKey || undefined,
      input: rawText.slice(0, 4096),
      model: speechConfig.openaiModel,
      voice: speechConfig.openaiVoice,
      format: speechConfig.openaiFormat,
      speed: speechConfig.speechRate,
      signal,
    })
  }

  function getOrStartOpenAIAudioRequest(rawText: string, signal?: AbortSignal) {
    const key = buildOpenAIAudioCacheKey(rawText)
    const existing = inFlightOpenAIAudioRequests.get(key)
    if (existing) {
      return { key, promise: existing }
    }

    const promise = fetchOpenAIAudioBlob(rawText, signal).finally(() => {
      if (inFlightOpenAIAudioRequests.get(key) === promise) {
        inFlightOpenAIAudioRequests.delete(key)
      }
    })
    inFlightOpenAIAudioRequests.set(key, promise)
    return { key, promise }
  }

  function clearPreloadedOpenAIAudio() {
    preloadGeneration += 1
    inFlightPreloadKeys.clear()
    inFlightOpenAIAudioRequests.clear()
    preloadedOpenAIAudio.value = []
  }

  async function preloadOpenAITTS(rawText?: string | string[] | null) {
    if (speechConfig.provider !== 'openai' || !openAISpeechConfigured.value) return
    const texts = Array.isArray(rawText) ? rawText : [rawText || '']
    const normalizedTexts = texts.map((item) => item.trim()).filter(Boolean)
    if (!normalizedTexts.length) return
    const pendingTexts = normalizedTexts.filter((item) => {
      const key = buildOpenAIAudioCacheKey(item)
      return !preloadedOpenAIAudio.value.some((entry) => entry.key === key) && !inFlightPreloadKeys.has(key)
    })
    if (!pendingTexts.length) return

    const generation = preloadGeneration
    for (const text of pendingTexts.slice(0, OPENAI_AUDIO_PRELOAD_LIMIT)) {
      const key = buildOpenAIAudioCacheKey(text)
      inFlightPreloadKeys.add(key)
      const { promise } = getOrStartOpenAIAudioRequest(text)
      void promise
        .then((blob) => {
          if (generation !== preloadGeneration) return
          const nextQueue = preloadedOpenAIAudio.value.filter((entry) => entry.key !== key)
          nextQueue.push({ key, blob })
          preloadedOpenAIAudio.value = nextQueue
        })
        .catch(() => undefined)
        .finally(() => {
          inFlightPreloadKeys.delete(key)
        })
    }
  }

  function stopOpenAIAudioPlayback() {
    if (currentOpenAIAbortController) {
      currentOpenAIAbortController.abort()
      currentOpenAIAbortController = null
    }
    if (currentOpenAIAudio) {
      currentOpenAIAudio.onplay = null
      currentOpenAIAudio.onpause = null
      currentOpenAIAudio.onended = null
      currentOpenAIAudio.onerror = null
      currentOpenAIAudio.pause()
      currentOpenAIAudio.src = ''
      currentOpenAIAudio = null
    }
    if (currentOpenAIAudioUrl) {
      URL.revokeObjectURL(currentOpenAIAudioUrl)
      currentOpenAIAudioUrl = ''
    }
  }

  function clearSpeechStopTimer(resetConfig = true) {
    if (speechStopTimer) {
      clearTimeout(speechStopTimer)
      speechStopTimer = null
    }
    speechStopAt.value = 0
    if (resetConfig) {
      speechConfig.stopAfterMinutes = 0
      saveSpeechConfig()
    }
  }

  function setSpeechStopTimer(minutes: number) {
    clearSpeechStopTimer(false)
    const normalized = Math.max(0, Math.min(180, Math.round(minutes)))
    speechConfig.stopAfterMinutes = normalized
    saveSpeechConfig()
    if (!normalized) {
      speechStopAt.value = 0
      return
    }
    speechStopAt.value = Date.now() + normalized * 60 * 1000
    speechStopTimer = window.setTimeout(() => {
      stopTTS()
      clearSpeechStopTimer(false)
      speechConfig.stopAfterMinutes = 0
      saveSpeechConfig()
      appStore.showToast('朗读已按定时设置停止', 'success')
    }, normalized * 60 * 1000)
  }

  function startSystemTTS(rawText: string, options: TTSOptions, sessionId: number) {
    if (!synth) return
    // 进入即转加载态：onstart 是异步的，否则点了「开始」按钮文案会闪回「开始」
    isSpeechLoading.value = true
    if (!voiceList.value.length) {
      fetchVoices()
    }

    const utterance = new SpeechSynthesisUtterance(rawText)
    currentUtterance = utterance
    const safariSpeechFallback = isSafariSpeechFallbackMode() && !systemTtsNativeEventsReliable.value

    const selectedVoice = voiceList.value.find((voice) => voice.name === speechConfig.voiceName)
    utterance.lang = selectedVoice?.lang || 'zh-CN'
    utterance.voice = selectedVoice || null
    utterance.rate = speechConfig.speechRate
    utterance.pitch = speechConfig.speechPitch
    logTTS('system speak queued', {
      sessionId,
      voice: utterance.voice?.name || utterance.lang,
      rate: utterance.rate,
      pitch: utterance.pitch,
      text: rawText.slice(0, 80),
    })

    let completed = false
    let finishWatchdog: number | null = null
    const startedAt = Date.now()
    let lastProgressAt = startedAt
    let sawStart = false
    let sawBoundary = false
    let pausedStartedAt: number | null = null
    let pausedAccumulatedMs = 0

    const clearFinishWatchdog = () => {
      if (finishWatchdog) {
        clearTimeout(finishWatchdog)
        finishWatchdog = null
      }
    }

    const effectiveElapsed = () => {
      const now = Date.now()
      const currentPaused = pausedStartedAt ? now - pausedStartedAt : 0
      return now - startedAt - pausedAccumulatedMs - currentPaused
    }

    const finalizePlayback = (kind: 'end' | 'error' | 'interrupted', event?: SpeechSynthesisErrorEvent) => {
      if (completed) return
      completed = true
      clearFinishWatchdog()
      if (currentUtterance === utterance) {
        currentUtterance = null
      }
      if (!isCurrentTTSSession(sessionId)) return
      isSpeaking.value = false
      isPaused.value = false
      isSpeechLoading.value = false
      logTTS('system finalize', {
        sessionId,
        kind,
        error: event?.error,
        speaking: synth?.speaking,
        pending: synth?.pending,
      })
      if (kind === 'end') {
        options.onEnd?.()
        return
      }
      if (kind === 'error') {
        options.onError?.(event)
      }
    }

    const forceFinalizeEnd = (reason: string) => {
      logTTS('system watchdog force end', {
        sessionId,
        reason,
        speaking: synth?.speaking,
        pending: synth?.pending,
        elapsed: effectiveElapsed(),
        text: rawText.slice(0, 40),
      })
      finalizePlayback('end')
      window.setTimeout(() => {
        if (!isCurrentTTSSession(sessionId)) return
        try {
          synth?.cancel()
        } catch {
          // ignore platform-specific cancel errors
        }
      }, 0)
    }

    const scheduleFinishWatchdog = () => {
      clearFinishWatchdog()
      const estimatedMs = safariSpeechFallback
        ? Math.max(2400, Math.ceil((rawText.length / Math.max(0.6, speechConfig.speechRate)) * 235))
        : Math.max(2800, Math.ceil((rawText.length / Math.max(0.6, speechConfig.speechRate)) * 280))
      const noStartTimeoutMs = safariSpeechFallback
        ? estimatedMs + Math.max(400, Math.ceil(rawText.length * 22))
        : 0
      const hardTimeoutMs = safariSpeechFallback
        ? estimatedMs + Math.max(1800, Math.ceil(rawText.length * 80))
        : Math.min(120000, estimatedMs + Math.max(4000, Math.ceil(rawText.length * 120)))
      logTTS('system watchdog scheduled', {
        sessionId,
        estimatedMs,
        noStartTimeoutMs,
        hardTimeoutMs,
        safariSpeechFallback,
        text: rawText.slice(0, 40),
      })
      const checkFinish = () => {
        if (completed || !isCurrentTTSSession(sessionId) || currentUtterance !== utterance) return
        if (synth?.paused || isPaused.value) {
          if (pausedStartedAt == null) {
            pausedStartedAt = Date.now()
          }
          lastProgressAt = Date.now()
          finishWatchdog = window.setTimeout(checkFinish, 600)
          return
        }
        if (pausedStartedAt != null) {
          pausedAccumulatedMs += Date.now() - pausedStartedAt
          pausedStartedAt = null
        }
        const elapsed = effectiveElapsed()
        const idleMs = Date.now() - lastProgressAt
        if (!synth?.speaking && !synth?.pending) {
          logTTS('system watchdog finalize end', { sessionId })
          finalizePlayback('end')
          return
        }
        if (sawBoundary && idleMs > 1800 && elapsed > Math.max(2200, estimatedMs * 0.75)) {
          forceFinalizeEnd('boundary-idle')
          return
        }
        if (safariSpeechFallback && !sawStart && elapsed > noStartTimeoutMs) {
          forceFinalizeEnd('no-start-timeout')
          return
        }
        if (elapsed > hardTimeoutMs) {
          forceFinalizeEnd('hard-timeout')
          return
        }
        finishWatchdog = window.setTimeout(checkFinish, 600)
      }
      finishWatchdog = window.setTimeout(checkFinish, safariSpeechFallback ? Math.min(estimatedMs, 1200) : estimatedMs)
    }

    utterance.onstart = () => {
      if (!isCurrentTTSSession(sessionId) || currentUtterance !== utterance) return
      isSpeechLoading.value = false
      isSpeaking.value = true
      isPaused.value = false
      sawStart = true
      systemTtsNativeEventsReliable.value = true
      lastProgressAt = Date.now()
      logTTS('system onstart', { sessionId, text: rawText.slice(0, 40) })
      options.onStart?.()
    }
    utterance.onboundary = () => {
      if (!isCurrentTTSSession(sessionId) || currentUtterance !== utterance) return
      sawBoundary = true
      lastProgressAt = Date.now()
    }
    utterance.onend = () => {
      logTTS('system onend', { sessionId, text: rawText.slice(0, 40) })
      finalizePlayback('end')
    }
    utterance.onerror = (event) => {
      const interrupted = event.error === 'interrupted' || event.error === 'canceled'
      logTTS('system onerror', { sessionId, error: event.error, interrupted, text: rawText.slice(0, 40) })
      finalizePlayback(interrupted ? 'interrupted' : 'error', event)
    }

    synth.speak(utterance)
    logTTS('system speak invoked', {
      sessionId,
      speaking: synth.speaking,
      pending: synth.pending,
      text: rawText.slice(0, 40),
    })
    scheduleFinishWatchdog()
  }

  async function startOpenAITTS(rawText: string, options: TTSOptions, sessionId: number) {
    if (!openAISpeechConfigured.value) {
      const error = new Error('请先配置 OpenAI Speech')
      appStore.showToast(error.message, 'warning')
      options.onError?.(error)
      return
    }
    if (speechConfig.openaiSource === 'server') {
      const serverConfig = await aiBookStore.loadServerModelConfig()
      if (!serverConfig?.canUseServerModel) {
        const error = new Error('后端模型配置暂不可用，请先登录')
        appStore.showToast(error.message, 'warning')
        options.onError?.(error)
        return
      }
      if (!serverConfig.config.speech.enabled) {
        const error = new Error('后端 OpenAI Speech 未启用')
        appStore.showToast(error.message, 'warning')
        options.onError?.(error)
        return
      }
      if (!isCurrentTTSSession(sessionId)) return
    }

    isSpeechLoading.value = true
    logTTS('openai speak queued', {
      sessionId,
      model: speechConfig.openaiModel,
      voice: speechConfig.openaiVoice,
      text: rawText.slice(0, 80),
    })
    const playBlob = (blob: Blob, controller: AbortController) => {
      if (controller.signal.aborted) return
      if (!isCurrentTTSSession(sessionId)) return
      isSpeechLoading.value = false
      currentOpenAIAudioUrl = URL.createObjectURL(blob)
      const audio = new Audio(currentOpenAIAudioUrl)
      currentOpenAIAudio = audio
      currentOpenAIAbortController = null

      audio.onplay = () => {
        if (!isCurrentTTSSession(sessionId) || currentOpenAIAudio !== audio) return
        isSpeaking.value = true
        isPaused.value = false
        logTTS('openai onplay', { sessionId, text: rawText.slice(0, 40) })
        options.onStart?.()
      }

      audio.onpause = () => {
        if (!isCurrentTTSSession(sessionId) || currentOpenAIAudio !== audio) return
        if (!audio.ended) {
          isPaused.value = true
          isSpeaking.value = false
        }
      }

      audio.onended = () => {
        if (currentOpenAIAudio === audio) {
          currentOpenAIAudio = null
        }
        if (!isCurrentTTSSession(sessionId)) return
        isSpeaking.value = false
        isPaused.value = false
        logTTS('openai onended', { sessionId, text: rawText.slice(0, 40) })
        if (currentOpenAIAudioUrl) {
          URL.revokeObjectURL(currentOpenAIAudioUrl)
          currentOpenAIAudioUrl = ''
        }
        options.onEnd?.()
      }

      audio.onerror = () => {
        if (currentOpenAIAudio === audio) {
          currentOpenAIAudio = null
        }
        if (!isCurrentTTSSession(sessionId)) return
        isSpeaking.value = false
        isPaused.value = false
        const error = new Error('OpenAI Speech 音频播放失败')
        logTTS('openai onerror', { sessionId, text: rawText.slice(0, 40) })
        options.onError?.(error)
      }

      return audio.play().catch((error: Error) => {
        if (!isCurrentTTSSession(sessionId)) return
        isSpeechLoading.value = false
        isSpeaking.value = false
        isPaused.value = false
        currentOpenAIAudio = null
        logTTS('openai play catch', { sessionId, message: error.message, text: rawText.slice(0, 40) })
        options.onError?.(error)
      })
    }

    const controller = new AbortController()
    currentOpenAIAbortController = controller

    const key = buildOpenAIAudioCacheKey(rawText)
    const cached = preloadedOpenAIAudio.value.find((entry) => entry.key === key)
    if (cached) {
      void Promise.resolve(playBlob(cached.blob, controller))
      return
    }

    const inFlight = inFlightOpenAIAudioRequests.get(key)
    if (inFlight) {
      void inFlight.then((blob) => {
        return playBlob(blob, controller)
      }).catch((error: Error) => {
        if (controller.signal.aborted || !isCurrentTTSSession(sessionId)) return
        isSpeechLoading.value = false
        isSpeaking.value = false
        isPaused.value = false
        currentOpenAIAbortController = null
        currentOpenAIAudio = null
        logTTS('openai inflight catch', { sessionId, message: error.message, text: rawText.slice(0, 40) })
        options.onError?.(error)
      })
      return
    }

    const started = getOrStartOpenAIAudioRequest(rawText, controller.signal)
    void started.promise.then((blob) => {
      return playBlob(blob, controller)
    }).catch((error: Error) => {
      if (controller.signal.aborted || !isCurrentTTSSession(sessionId)) return
      isSpeechLoading.value = false
      isSpeaking.value = false
      isPaused.value = false
      currentOpenAIAbortController = null
      currentOpenAIAudio = null
      logTTS('openai request catch', { sessionId, message: error.message, text: rawText.slice(0, 40) })
      appStore.showToast(error.message || 'OpenAI Speech 请求失败', 'error')
      options.onError?.(error)
    })
  }

  function startTTS(text?: string, options: TTSOptions = {}, interruptCurrent = true) {
    const hasActiveSystemSpeech = !!synth && (synth.speaking || synth.pending || !!currentUtterance)
    const hasActiveOpenAISpeech = !!currentOpenAIAudio || !!currentOpenAIAbortController

    if (interruptCurrent && (hasActiveSystemSpeech || hasActiveOpenAISpeech || isSpeaking.value || isSpeechLoading.value)) {
      stopTTS(false)
    }

    const rawText = (text || content.value.replace(/<[^>]+>/g, '')).trim()
    if (!rawText) return

    const sessionId = beginTTSSession()
    logTTS('startTTS', {
      sessionId,
      provider: speechConfig.provider,
      interruptCurrent,
      text: rawText.slice(0, 80),
    })

    if (
      !interruptCurrent &&
      speechConfig.provider === 'system' &&
      synth &&
      !synth.speaking &&
      isSafariSpeechFallbackMode() &&
      !systemTtsNativeEventsReliable.value
    ) {
      try {
        logTTS('startTTS cleanup idle system synth', { sessionId })
        synth.cancel()
      } catch {
        // ignore platform-specific cancel errors
      }
    }

    if (speechConfig.provider === 'openai') {
      void startOpenAITTS(rawText, options, sessionId)
      return
    }

    startSystemTTS(rawText, options, sessionId)
  }

  function pauseTTS() {
    if (speechConfig.provider === 'openai') {
      if (!currentOpenAIAudio) return
      if (currentOpenAIAudio.paused) {
        void currentOpenAIAudio.play()
        isPaused.value = false
        isSpeaking.value = true
      } else {
        currentOpenAIAudio.pause()
        isPaused.value = true
        isSpeaking.value = false
      }
      return
    }

    if (!synth) return
    if (synth.speaking && !synth.paused) {
      synth.pause()
      isPaused.value = true
    } else if (synth.paused) {
      synth.resume()
      isPaused.value = false
    }
  }

  function stopTTS(resetCallbacks = true) {
    const sessionId = beginTTSSession()
    logTTS('stopTTS', {
      sessionId,
      resetCallbacks,
      provider: speechConfig.provider,
      caller: captureTTSCaller(),
    })
    if (synth) {
      synth.cancel()
      currentUtterance = null
    }
    stopOpenAIAudioPlayback()
    isSpeechLoading.value = false
    isSpeaking.value = false
    isPaused.value = false
    if (resetCallbacks) {
      clearSpeechStopTimer()
    }
  }

  return {
    isSpeaking, isSpeechLoading, isPaused, startTTS, pauseTTS, stopTTS,
    voiceList, speechConfig, speechStopAt, speechProviderLabel, openAISpeechConfigured,
    systemTtsNativeEventsReliable,
    fetchVoices, setVoiceName, setSpeechProvider, setSpeechRate, setSpeechPitch, setSpeechStopTimer, clearSpeechStopTimer,
    setOpenAISpeechSource, setOpenAISpeechBaseUrl, setOpenAISpeechApiKey, setOpenAISpeechModel, setOpenAISpeechVoice, setOpenAISpeechFormat, setOpenAISpeechRequestMode, preloadOpenAITTS,
  }
}
