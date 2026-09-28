import { watch } from 'vue'
import type { ComputedRef, Ref } from 'vue'
import type { useReaderStore } from '../stores/reader'

type ReaderStore = ReturnType<typeof useReaderStore>
const OPENAI_SPEECH_CHUNK_CHAR_LIMIT = 70
const OPENAI_PRELOAD_CHUNK_LIMIT = 5
const OPENAI_MERGED_SEGMENT_CHAR_LIMIT = 260

interface AutoPlaybackConfig {
  autoPageMode: string
  clickAction: string
  scrollPixel: number
  pageSpeed: number
  fontSize: number
  lineHeight: number
}

export function useReaderAutoPlayback(
  store: ReaderStore,
  config: ComputedRef<AutoPlaybackConfig>,
  isContinuousMode: ComputedRef<boolean>,
  scrollContainerRef: Ref<HTMLElement | undefined>,
  chapterTextRef: Ref<HTMLElement | undefined>,
  nextChapter: () => void | Promise<void>,
  prevChapter: () => void | Promise<void>,
) {
  let autoScrollId: number | null = null
  let autoParagraphTimer: number | null = null
  let autoReadingParagraphIndex = -1
  let autoReadingProcessing = false
  let speechRestartTimer: number | null = null
  let isSpeechTransitioning = false
  // 朗读链路自己驱动的翻章（章末续读/跨章上一段）也会改 content；
  // handleContentChanged 据此区分「用户手动翻章」——只有后者才把朗读重定位到新章
  let speechDrivenChapterChange = false
  let currentSpeechParagraph: HTMLElement | null = null
  let currentSpeechSegments: { text: string; nextParagraph: HTMLElement | null }[] = []
  let currentSpeechSegmentIndex = 0

  function isSafariSpeechDelayBrowser() {
    if (typeof navigator === 'undefined') return false
    const ua = navigator.userAgent || ''
    return /Safari/i.test(ua) && !/Chrome|Chromium|CriOS|Edg|EdgiOS|Android/i.test(ua)
  }

  function paragraphPreview(paragraph: HTMLElement | null) {
    return paragraph?.innerText.trim().slice(0, 40) || ''
  }

  function logSpeech(message: string, payload?: unknown) {
    void message
    void payload
  }

  // 段落列表缓存：每步朗读/自动翻页都全章 querySelectorAll + 逐段 innerText
  //（布局依赖属性，强制样式计算）。DOM 重建（切章/改排版）时失效；
  // 再配 isConnected 自检，兜住「v-html 换掉了段落元素」的边角。
  let paragraphCache: HTMLElement[] | null = null

  function getFilteredParagraphs() {
    if (paragraphCache && paragraphCache.length && paragraphCache[0].isConnected) {
      return paragraphCache
    }
    const roots = isContinuousMode.value
      ? Array.from(scrollContainerRef.value?.querySelectorAll('.chapter-text[data-role="continuous"]') || []) as HTMLElement[]
      : (chapterTextRef.value ? [chapterTextRef.value] : [])
    if (!roots.length) return [] as HTMLElement[]
    const allElements = roots.flatMap((root) => {
      const paragraphs = Array.from(root.querySelectorAll('p')) as HTMLElement[]
      if (paragraphs.length) return paragraphs
      return root.innerText.trim() ? [root] : []
    })
    const list: HTMLElement[] = []
    let lastText = ''
    allElements.forEach((el) => {
      const text = el.innerText.trim()
      if (text && text !== lastText) {
        list.push(el)
        lastText = text
      }
    })
    paragraphCache = list
    return list
  }

  // DOM 重建时失效段落缓存（切章 / 改排版 / 繁简切换都会换掉段落元素）
  watch(
    [() => store.currentIndex, () => store.content, () => store.displayContent, () => config.value.fontSize, () => config.value.lineHeight, isContinuousMode],
    () => {
      paragraphCache = null
    },
  )

  function getCurrentParagraph() {
    const reading = chapterTextRef.value?.querySelector('.reading') as HTMLElement | null
    if (reading) return reading

    const container = scrollContainerRef.value
    if (!container) return null

    const list = getFilteredParagraphs()
    for (const paragraph of list) {
      const top = paragraph.offsetTop - container.scrollTop
      const bottom = top + paragraph.offsetHeight
      if (bottom > 40) {
        return paragraph
      }
    }

    return list[0] || null
  }

  function getPrevParagraph() {
    const current = getCurrentParagraph()
    return getPrevParagraphFrom(current)
  }

  function getPrevParagraphFrom(current: HTMLElement | null) {
    const list = getFilteredParagraphs()
    const index = current ? list.indexOf(current) : -1
    if (index > 0) return list[index - 1]
    return null
  }

  function getNextParagraph() {
    const current = getCurrentParagraph()
    return getNextParagraphFrom(current)
  }

  function getNextParagraphFrom(current: HTMLElement | null) {
    const list = getFilteredParagraphs()
    const index = current ? list.indexOf(current) : -1
    if (index >= 0 && index < list.length - 1) return list[index + 1]
    return null
  }

  // 正文 DOM 会被评论气泡注入/简繁转换整棵重建：旧段元素失效 ≠ 章末。
  // 朗读链路上下一步拿到的若是已 detach 的旧元素，按滚动位置重新锚定
  //（showParagraph 已把刚读完的那段滚到顶部，其下一段即原定目标），
  // 否则链路会把「重建」误判成「章末」而提前翻到下一章
  function reanchorIfDetached(paragraph: HTMLElement | null) {
    if (paragraph && !paragraph.isConnected) {
      return getNextParagraphFrom(getCurrentParagraph())
    }
    return paragraph
  }

  // 朗读文本去掉段评气泡的计数按钮（innerText 会把「999+」也读出来）
  function paragraphSpeakText(paragraph: HTMLElement | null | undefined) {
    if (!paragraph) return ''
    const clone = paragraph.cloneNode(true) as HTMLElement
    clone.querySelectorAll('.para-comment-bubble').forEach((el) => el.remove())
    return (clone.textContent || '').trim()
  }

  function splitLongSentence(sentence: string) {
    const chunks: string[] = []
    let remaining = sentence.trim()
    while (remaining.length > OPENAI_SPEECH_CHUNK_CHAR_LIMIT) {
      let splitIndex = Math.max(
        remaining.lastIndexOf('，', OPENAI_SPEECH_CHUNK_CHAR_LIMIT),
        remaining.lastIndexOf('、', OPENAI_SPEECH_CHUNK_CHAR_LIMIT),
        remaining.lastIndexOf(',', OPENAI_SPEECH_CHUNK_CHAR_LIMIT),
        remaining.lastIndexOf(' ', OPENAI_SPEECH_CHUNK_CHAR_LIMIT),
      )
      if (splitIndex <= 0) {
        splitIndex = OPENAI_SPEECH_CHUNK_CHAR_LIMIT
      }
      chunks.push(remaining.slice(0, splitIndex).trim())
      remaining = remaining.slice(splitIndex).trim()
    }
    if (remaining) chunks.push(remaining)
    return chunks
  }

  function buildParagraphSpeechChunks(paragraph: HTMLElement | null) {
    const rawText = paragraphSpeakText(paragraph)
    if (!rawText) return [] as string[]

    const sentences = rawText
      .replace(/\n+/g, '\n')
      .split(/(?<=[。！？!?；;])/)
      .map((item) => item.trim())
      .filter(Boolean)

    const chunks: string[] = []
    let current = ''

    const pushCurrent = () => {
      const normalized = current.trim()
      if (normalized) chunks.push(normalized)
      current = ''
    }

    for (const sentence of (sentences.length ? sentences : [rawText])) {
      if (sentence.length > OPENAI_SPEECH_CHUNK_CHAR_LIMIT) {
        pushCurrent()
        chunks.push(...splitLongSentence(sentence))
        continue
      }
      const next = current ? `${current}${sentence}` : sentence
      if (next.length > OPENAI_SPEECH_CHUNK_CHAR_LIMIT) {
        pushCurrent()
        current = sentence
      } else {
        current = next
      }
    }

    pushCurrent()
    return chunks.length ? chunks : [rawText]
  }

  function buildMergedSpeechSegment(paragraph: HTMLElement | null) {
    const currentText = paragraphSpeakText(paragraph)
    if (!currentText) {
      return {
        text: '',
        nextParagraph: getNextParagraph(),
      }
    }

    const list = getFilteredParagraphs()
    const startIndex = paragraph ? list.indexOf(paragraph) : -1
    if (startIndex < 0) {
      return {
        text: currentText,
        nextParagraph: getNextParagraph(),
      }
    }

    const mergedTexts: string[] = [currentText]
    let mergedLength = currentText.length
    let cursorIndex = startIndex + 1

    while (cursorIndex < list.length && mergedLength < OPENAI_MERGED_SEGMENT_CHAR_LIMIT) {
      const nextText = paragraphSpeakText(list[cursorIndex])
      if (!nextText) {
        cursorIndex += 1
        continue
      }
      if (mergedLength + nextText.length > OPENAI_MERGED_SEGMENT_CHAR_LIMIT) {
        break
      }
      mergedTexts.push(nextText)
      mergedLength += nextText.length
      cursorIndex += 1
    }

    return {
      text: mergedTexts.join('\n'),
      nextParagraph: list[cursorIndex] || null,
    }
  }

  function resetSpeechChunkState() {
    currentSpeechParagraph = null
    currentSpeechSegments = []
    currentSpeechSegmentIndex = 0
  }

  function buildOpenAISpeechSegments(paragraph: HTMLElement) {
    if (store.speechConfig.openaiRequestMode === 'merged') {
      const merged = buildMergedSpeechSegment(paragraph)
      return merged.text ? [merged] : []
    }

    const paragraphChunks = buildParagraphSpeechChunks(paragraph)
    const nextParagraph = getNextParagraph()
    return paragraphChunks.map((text, index) => ({
      text,
      nextParagraph: index < paragraphChunks.length - 1 ? paragraph : nextParagraph,
    }))
  }

  function ensureSpeechChunkState(paragraph: HTMLElement) {
    if (store.speechConfig.provider !== 'openai') {
      return {
        text: paragraphSpeakText(paragraph),
        nextParagraph: getNextParagraphFrom(paragraph),
      }
    }

    if (currentSpeechParagraph !== paragraph) {
      currentSpeechParagraph = paragraph
      currentSpeechSegments = buildOpenAISpeechSegments(paragraph)
      currentSpeechSegmentIndex = 0
    }

    return currentSpeechSegments[currentSpeechSegmentIndex] || {
      text: '',
      nextParagraph: getNextParagraphFrom(paragraph),
    }
  }

  function getUpcomingSpeechChunks(startParagraph: HTMLElement | null) {
    const chunks: string[] = []

    if (store.speechConfig.provider !== 'openai') {
      return chunks
    }

    if (store.speechConfig.openaiRequestMode === 'merged') {
      const merged = buildMergedSpeechSegment(startParagraph)
      return merged.text ? [merged.text] : []
    }

    if (currentSpeechParagraph && currentSpeechSegments.length) {
      for (let i = currentSpeechSegmentIndex + 1; i < currentSpeechSegments.length && chunks.length < OPENAI_PRELOAD_CHUNK_LIMIT; i += 1) {
        if (currentSpeechSegments[i]?.text) {
          chunks.push(currentSpeechSegments[i].text)
        }
      }
    }

    let cursor = startParagraph
    while (cursor && chunks.length < OPENAI_PRELOAD_CHUNK_LIMIT) {
      const paragraphChunks = buildParagraphSpeechChunks(cursor)
      for (const chunk of paragraphChunks) {
        if (chunks.length >= OPENAI_PRELOAD_CHUNK_LIMIT) break
        chunks.push(chunk)
      }
      const list = getFilteredParagraphs()
      const index = list.indexOf(cursor)
      cursor = index >= 0 ? (list[index + 1] || null) : null
    }

    return chunks
  }

  function clearReadingClass() {
    scrollContainerRef.value?.querySelectorAll('.reading').forEach((el) => el.classList.remove('reading'))
  }

  function showParagraph(paragraph: HTMLElement | null, smooth = true) {
    const container = scrollContainerRef.value
    if (!container || !paragraph) return

    const targetTop = Math.max(0, paragraph.offsetTop - 24)
    container.scrollTo({
      top: targetTop,
      behavior: smooth ? 'smooth' : 'auto',
    })
  }

  function markReadingParagraph(paragraph: HTMLElement | null) {
    clearReadingClass()
    if (paragraph) {
      paragraph.classList.add('reading')
    }
  }

  function runAutoScroll() {
    if (!store.isAutoScrolling || !scrollContainerRef.value) return

    const container = scrollContainerRef.value
    const speed = Math.max(1, config.value.scrollPixel) * (config.value.pageSpeed / 1000) * 0.5

    container.scrollTop += speed

    if (container.scrollTop + container.clientHeight >= container.scrollHeight - 2) {
      if (config.value.clickAction === 'auto' && store.hasNext) {
        void nextChapter()
      } else {
        stopAutoScroll()
      }
    } else {
      autoScrollId = requestAnimationFrame(runAutoScroll)
    }
  }

  function runAutoParagraph() {
    if (!store.isAutoScrolling) return
    if (autoReadingProcessing) return

    const list = getFilteredParagraphs()
    if (!list.length) return

    autoReadingProcessing = true

    if (autoReadingParagraphIndex < 0) {
      const current = getCurrentParagraph()
      autoReadingParagraphIndex = current ? Math.max(0, list.indexOf(current)) : 0
    }

    if (autoReadingParagraphIndex >= list.length) {
      autoReadingParagraphIndex = -1
      autoReadingProcessing = false
      if (store.hasNext) {
        Promise.resolve(nextChapter()).then(() => {
          window.setTimeout(() => {
            if (store.isAutoScrolling && config.value.autoPageMode === 'paragraph') {
              runAutoParagraph()
            }
          }, 300)
        })
      } else {
        stopAutoScroll()
      }
      return
    }

    const current = list[autoReadingParagraphIndex]
    markReadingParagraph(current)
    showParagraph(current)

    const estimatedLineCount = Math.max(1, Math.ceil(current.offsetHeight / (config.value.fontSize * config.value.lineHeight)))
    const delayTime = Math.max(300, config.value.pageSpeed * estimatedLineCount)

    autoReadingProcessing = false
    autoParagraphTimer = window.setTimeout(() => {
      autoReadingParagraphIndex += 1
      runAutoParagraph()
    }, delayTime)
  }

  function startAutoScroll() {
    if (config.value.autoPageMode === 'paragraph') {
      if (autoParagraphTimer) return
      runAutoParagraph()
      return
    }
    if (autoScrollId) return
    runAutoScroll()
  }

  function stopAutoScroll() {
    store.isAutoScrolling = false
    autoReadingParagraphIndex = -1
    autoReadingProcessing = false
    if (autoScrollId) {
      cancelAnimationFrame(autoScrollId)
      autoScrollId = null
    }
    if (autoParagraphTimer) {
      clearTimeout(autoParagraphTimer)
      autoParagraphTimer = null
    }
    if (!store.isSpeaking) {
      clearReadingClass()
    }
  }

  function restartSpeechTarget(paragraph: HTMLElement | null, interruptCurrent = true) {
    // 不做 reanchorIfDetached：这里的入参也可能是「上一段」，方向会反；
    // 其调用方的目标都取自实时 DOM，不会 detach
    logSpeech('restartSpeechTarget', {
      interruptCurrent,
      paragraph: paragraphPreview(paragraph),
      isSpeechTransitioning,
    })
    if (!paragraph) {
      store.stopTTS()
      resetSpeechChunkState()
      return
    }
    if (isSpeechTransitioning) return
    isSpeechTransitioning = true
    resetSpeechChunkState()
    if (interruptCurrent) {
      store.stopTTS(false)
    }
    if (speechRestartTimer) {
      clearTimeout(speechRestartTimer)
    }
    const restartDelay = !interruptCurrent && store.speechConfig.provider === 'system'
      ? ((isSafariSpeechDelayBrowser() && !store.systemTtsNativeEventsReliable) ? 160 : 40)
      : 150
    speechRestartTimer = window.setTimeout(() => {
      if (store.isPaused) {
        isSpeechTransitioning = false
        return
      }
      isSpeechTransitioning = false
      startSpeech(paragraph, interruptCurrent)
    }, restartDelay)
  }

  function continueSpeechTarget(paragraph: HTMLElement | null, resetChunks = true) {
    paragraph = reanchorIfDetached(paragraph)
    logSpeech('continueSpeechTarget', {
      resetChunks,
      paragraph: paragraphPreview(paragraph),
      hasNextChapter: store.hasNext,
    })
    if (speechRestartTimer) {
      clearTimeout(speechRestartTimer)
    }

    const continueDelay = store.speechConfig.provider === 'system'
      ? ((isSafariSpeechDelayBrowser() && !store.systemTtsNativeEventsReliable) ? 160 : 40)
      : 120

    if (paragraph) {
      isSpeechTransitioning = true
      if (resetChunks) {
        resetSpeechChunkState()
      }
      speechRestartTimer = window.setTimeout(() => {
        if (store.isPaused) {
          isSpeechTransitioning = false
          return
        }
        isSpeechTransitioning = false
        startSpeech(paragraph, false)
      }, continueDelay)
      return
    }

    if (!store.hasNext) {
      store.stopTTS()
      clearReadingClass()
      return
    }

    isSpeechTransitioning = true
    if (resetChunks) {
      resetSpeechChunkState()
    }
    speechDrivenChapterChange = true
    const fromIndex = store.currentIndex
    Promise.resolve(nextChapter())
      .then(() => {
        speechRestartTimer = window.setTimeout(() => {
          speechDrivenChapterChange = false
          if (store.isPaused) {
            isSpeechTransitioning = false
            return
          }
          isSpeechTransitioning = false
          if (store.currentIndex === fromIndex) {
            // 翻章失败（loadChapter 已 toast 并有错误态）：停播而不是在旧章上循环重试
            store.stopTTS()
            clearReadingClass()
            return
          }
          startSpeech(getFilteredParagraphs()[0] || null, false)
        }, continueDelay)
      })
      .catch(() => {
        speechDrivenChapterChange = false
        isSpeechTransitioning = false
      })
  }

  function startSpeech(paragraph?: HTMLElement | null, interruptCurrent = true) {
    const current = paragraph || getCurrentParagraph()
    logSpeech('startSpeech', {
      interruptCurrent,
      paragraph: paragraphPreview(current),
      currentIndex: store.currentIndex,
    })
    if (!current || !paragraphSpeakText(current)) {
      if (interruptCurrent) {
        speechNext()
      } else {
        continueSpeechTarget(getNextParagraph())
      }
      return
    }

    markReadingParagraph(current)
    showParagraph(current)
    const chunk = ensureSpeechChunkState(current)
    if (!chunk.text.trim()) {
      if (interruptCurrent) {
        speechNext(chunk.nextParagraph)
      } else {
        continueSpeechTarget(chunk.nextParagraph)
      }
      return
    }
    const nextParagraph = chunk.nextParagraph
    logSpeech('speak chunk', {
      interruptCurrent,
      provider: store.speechConfig.provider,
      text: chunk.text.slice(0, 60),
      nextParagraph: paragraphPreview(nextParagraph),
      chunkIndex: currentSpeechSegmentIndex,
      chunkCount: currentSpeechSegments.length,
    })
    store.startTTS(chunk.text, {
      onEnd: () => {
        logSpeech('chunk onEnd', {
          provider: store.speechConfig.provider,
          currentParagraph: paragraphPreview(current),
          nextParagraph: paragraphPreview(nextParagraph),
          chunkIndex: currentSpeechSegmentIndex,
          chunkCount: currentSpeechSegments.length,
        })
        if (store.speechConfig.provider === 'openai' && currentSpeechParagraph === current && currentSpeechSegmentIndex < currentSpeechSegments.length - 1) {
          currentSpeechSegmentIndex += 1
          continueSpeechTarget(current, false)
          return
        }
        continueSpeechTarget(nextParagraph)
      },
      onError: () => {
        logSpeech('chunk onError', {
          currentParagraph: paragraphPreview(current),
          nextParagraph: paragraphPreview(nextParagraph),
        })
        resetSpeechChunkState()
        clearReadingClass()
      },
    }, interruptCurrent)
    const preloadTexts = getUpcomingSpeechChunks(nextParagraph)
    if (preloadTexts.length) {
      window.setTimeout(() => {
        void store.preloadOpenAITTS(preloadTexts)
      }, 0)
    }
  }

  function speechPrev() {
    logSpeech('speechPrev', {
      currentParagraph: paragraphPreview(getCurrentParagraph()),
      hasPrevChapter: store.hasPrev,
    })
    resetSpeechChunkState()
    const prev = getPrevParagraph()
    if (prev) {
      restartSpeechTarget(prev)
      return
    }
    if (!store.hasPrev) {
      store.stopTTS()
      return
    }
    store.stopTTS(false)
    speechDrivenChapterChange = true
    const fromIndex = store.currentIndex
    Promise.resolve(prevChapter())
      .then(() => {
        window.setTimeout(() => {
          speechDrivenChapterChange = false
          if (store.currentIndex === fromIndex) {
            // 翻章失败：已 toast，停播而不是读旧章末尾再跳走
            store.stopTTS()
            clearReadingClass()
            return
          }
          const list = getFilteredParagraphs()
          restartSpeechTarget(list[list.length - 1] || null)
        }, 120)
      })
      .catch(() => {
        speechDrivenChapterChange = false
      })
  }

  function speechNext(forcedNext?: HTMLElement | null, interruptCurrent = true) {
    logSpeech('speechNext', {
      interruptCurrent,
      forcedNext: paragraphPreview(forcedNext || null),
      currentParagraph: paragraphPreview(getCurrentParagraph()),
      hasNextChapter: store.hasNext,
    })
    resetSpeechChunkState()
    const next = forcedNext ?? getNextParagraph()
    if (next) {
      restartSpeechTarget(next, interruptCurrent)
      return
    }
    if (!store.hasNext) {
      store.stopTTS()
      clearReadingClass()
      return
    }
    if (interruptCurrent) {
      store.stopTTS(false)
    }
    speechDrivenChapterChange = true
    const fromIndex = store.currentIndex
    Promise.resolve(nextChapter())
      .then(() => {
        window.setTimeout(() => {
          speechDrivenChapterChange = false
          if (store.currentIndex === fromIndex) {
            // 翻章失败：已 toast，停播而不是在旧章上循环
            store.stopTTS()
            clearReadingClass()
            return
          }
          restartSpeechTarget(getFilteredParagraphs()[0] || null)
        }, 120)
      })
      .catch(() => {
        speechDrivenChapterChange = false
      })
  }

  function restartSpeechFromCurrentParagraph() {
    logSpeech('restartSpeechFromCurrentParagraph', {
      currentParagraph: paragraphPreview(getCurrentParagraph()),
      isSpeechTransitioning,
    })
    if (isSpeechTransitioning) return
    isSpeechTransitioning = true
    resetSpeechChunkState()
    store.stopTTS(false)
    if (speechRestartTimer) {
      clearTimeout(speechRestartTimer)
    }
    speechRestartTimer = window.setTimeout(() => {
      if (store.isPaused) {
        isSpeechTransitioning = false
        return
      }
      isSpeechTransitioning = false
      startSpeech()
    }, 150)
  }

  function cancelSpeechTransition() {
    if (speechRestartTimer) {
      clearTimeout(speechRestartTimer)
      speechRestartTimer = null
    }
    isSpeechTransitioning = false
  }

  function resetAutoParagraphIndex() {
    autoReadingParagraphIndex = -1
  }

  function handleContentChanged() {
    autoReadingParagraphIndex = -1
    if (store.isAutoScrolling && config.value.autoPageMode === 'paragraph') {
      if (autoParagraphTimer) {
        clearTimeout(autoParagraphTimer)
        autoParagraphTimer = null
      }
      window.setTimeout(() => {
        if (store.isAutoScrolling && config.value.autoPageMode === 'paragraph') {
          runAutoParagraph()
        }
      }, 100)
    }

    // 朗读/待播中正文被换掉，且不是朗读链路自己翻的章：用户手动翻了章
    // （或从书架面板换了书）。旧章段落已随 v-html 重建 detach，继续播会
    // 读旧章文本、把新章滚回顶部，段末再触发一次翻章直接跳过用户打开的章。
    // 停掉旧段并重定位到新章开头；暂停态只停旧段不自动开播（resume 落到新章）
    const speechActive = store.isSpeaking || store.isPaused || isSpeechTransitioning || speechRestartTimer !== null
    if (!speechDrivenChapterChange && speechActive) {
      const wasPaused = store.isPaused
      cancelSpeechTransition()
      resetSpeechChunkState()
      clearReadingClass()
      store.stopTTS(false)
      if (!wasPaused) {
        speechRestartTimer = window.setTimeout(() => {
          speechRestartTimer = null
          startSpeech(getFilteredParagraphs()[0] || null, false)
        }, 150)
      }
    }
  }

  function disposeAutoPlayback() {
    cancelSpeechTransition()
    stopAutoScroll()
  }

  return {
    getCurrentParagraph,
    clearReadingClass,
    startAutoScroll,
    stopAutoScroll,
    startSpeech,
    speechPrev,
    speechNext,
    restartSpeechFromCurrentParagraph,
    cancelSpeechTransition,
    resetAutoParagraphIndex,
    handleContentChanged,
    disposeAutoPlayback,
  }
}
