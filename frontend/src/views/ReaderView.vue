<template>
  <div
    class="reader-view"
    :class="{ 'disable-system-callout': disableSystemCallout }"
    :style="{
      background: theme.body,
      color: theme.fontColor,
      fontFamily: currentFontFamily,
      '--color-primary': '#c97f3a'
    }"
    @click="handleBackgroundClick"
    @contextmenu.prevent="handleContextMenu"
  >
    <!-- Left Drawer Panels -->
    <Teleport to="body">
      <Transition name="fade">
        <div v-if="store.activePanel" class="reader-overlay" @click="store.closePanel()"></div>
      </Transition>
      <Transition name="slide-left">
        <div v-if="store.activePanel" class="reader-drawer" :style="{ background: chromeTheme.popup }">
          <ReaderCatalog
            v-if="store.activePanel === 'catalog' || store.activePanel === 'bookmark'"
            :initial-tab="store.activePanel === 'bookmark' ? 'bookmarks' : 'chapters'"
            @jump-chapter="jumpFromCatalog"
          />
          <ReadSettings v-else-if="store.activePanel === 'settings'" />
          <ReaderBookshelf v-else-if="store.activePanel === 'bookshelf'" />
          <ReaderSource v-else-if="store.activePanel === 'source'" />
          <ReplaceRuleManager v-else-if="store.activePanel === 'rule'" />
          <CacheManager v-else-if="store.activePanel === 'cache'" />
        </div>
      </Transition>
    </Teleport>

    <!-- PC Desktop Toolbars (Always shown) -->
    <ReaderSidebar
      v-if="!isMobile"
      @goBack="goBack"
      @scrollTop="scrollToTop"
      @scrollBottom="scrollToBottom"
    />
    <ReaderToolbar
      v-if="!isMobile"
      :is-speaking="store.isSpeaking"
      :is-paused="store.isPaused"
      @bookmark="toggleBookmark"
      @search="toggleSearch"
      @info="openInfo"
      @ai="openAiBook"
      @tts="handleTTS"
      @prev="prevChapter"
      @next="nextChapter"
      @progress="openCachePanel"
    />

    <!-- Mobile Controls (Click to toggle) -->
    <ReaderMobileControls
      v-if="isMobile"
      :show="showControls || !!store.activePanel"
      @goBack="goBack"
      @scrollTop="scrollToTop"
      @scrollBottom="scrollToBottom"
      @prev="prevChapter"
      @next="nextChapter"
      @bookmark="toggleBookmark"
      @search="openSearch"
      @info="openInfo"
      @ai="openAiBook"
      @tts="handleTTS"
      @progress="openCachePanel"
    />

    <ReaderTtsPanel
      :show="showTTSPanel"
      :theme="chromeTheme"
      :chapter-title="store.currentChapter?.title"
      :provider="store.speechConfig.provider"
      :provider-label="store.speechProviderLabel"
      :is-speaking="store.isSpeaking"
      :is-loading="store.isSpeechLoading"
      :is-paused="store.isPaused"
      :voices="store.voiceList"
      :voice-name="store.speechConfig.voiceName"
      :rate="store.speechConfig.speechRate"
      :pitch="store.speechConfig.speechPitch"
      :supports-pitch="store.speechConfig.provider === 'system'"
      :openai-model="store.speechConfig.openaiModel"
      :openai-voice="store.speechConfig.openaiVoice"
      :openai-source="store.speechConfig.openaiSource"
      :stop-after-minutes="store.speechConfig.stopAfterMinutes"
      :timer-text="speechTimerText"
      @close="closeTTSPanel"
      @prev="speechPrev"
      @toggle-play="toggleSpeechFromPanel"
      @stop="handleStopTTS"
      @next="speechNext"
      @voice-change="changeVoice"
      @openai-voice-change="changeOpenAIVoice"
      @rate-change="adjustSpeechRate"
      @pitch-change="adjustSpeechPitch"
      @timer-change="setSpeechTimer"
    />

    <CommentPanel
      :show="showCommentPanel"
      :theme="chromeTheme"
      :mode="commentMode"
      :book-url="store.book?.bookUrl || ''"
      :chapter-url="store.currentChapter?.url || ''"
      :book-source-url="store.book?.origin"
      :para-index="commentParaIndex"
      :para-text="commentParaText"
      :initial-page="store.chapterComments"
      :author-marks="store.reviewAuthorMarks"
      :para-author-marked="commentParaAuthorMarked"
      @close="closeCommentPanel"
    />

    <!-- Main Content Area -->
    <div
      class="reader-scroll-container"
      :class="{ 'horizontal-page-mode': isHorizontalPageMode }"
      ref="scrollContainerRef"
      @scroll="handleScroll"
      @mousedown="stopAutoScroll"
      @touchstart="handleTouchStart"
      @touchmove="handleTouchMove"
      @touchend="handleTouchEnd"
      @click="handleGlobalClick"
    >
      <div v-if="store.loading" class="content-loading">
        <div class="loading-spinner"></div>
      </div>

      <div v-else-if="offlineBannerText" class="offline-banner">
        {{ offlineBannerText }}
      </div>

      <article
        v-if="!store.loading && !isContinuousMode"
        class="chapter-content"
        :class="{ 'horizontal-page-article': isHorizontalPageMode }"
        :style="{
          maxWidth: isHorizontalPageMode ? 'none' : (config.pageWidth + 'px'),
          fontSize: config.fontSize + 'px',
          fontWeight: config.fontWeight,
          lineHeight: config.lineHeight,
          '--reader-page-width': config.pageWidth + 'px',
          '--reader-side-padding': '24px',
          '--reader-page-step': horizontalPageStepStyle,
        }"
      >
        <div v-if="isHorizontalPageMode" class="horizontal-page-layout">
          <div v-if="store.loadError" class="load-error">
            <p>{{ store.loadError }}</p>
            <button class="retry-btn" @click="store.retryLoad()">重试</button>
          </div>
          <section v-else class="horizontal-content-page">
            <div
              ref="chapterTextRef"
              class="horizontal-pages"
              :style="{
                transform: horizontalPageTransform,
                transitionDuration: horizontalPageTransitionDuration,
              }"
            >
              <section v-for="(page, idx) in horizontalPages" :key="`h-page-${idx}`" class="horizontal-page">
                <div
                  class="chapter-text horizontal-page-content"
                  :style="{
                    '--p-spacing': config.paragraphSpacing + 'em',
                  }"
                  v-html="page"
                  @click="handleChapterTextClick"
                ></div>
              </section>
            </div>
          </section>
        </div>

        <div v-else>
          <div v-if="store.loadError" class="load-error">
            <p>{{ store.loadError }}</p>
            <button class="retry-btn" @click="store.retryLoad()">重试</button>
          </div>
          <template v-else>
            <div class="chapter-title">{{ store.currentChapter?.title || '加载中...' }}</div>

            <div
              ref="chapterTextRef"
              class="chapter-text"
              :style="{
                '--p-spacing': config.paragraphSpacing + 'em',
              }"
              v-html="formattedContent"
              @click="handleChapterTextClick"
            ></div>

            <button
              v-if="store.reviewEnabled"
              class="chapter-comments-bar"
              @click="openChapterComments"
            >
              <span class="chapter-comments-label">本章评论</span>
              <span v-if="store.chapterCommentTotal > 0" class="chapter-comments-count">
                · {{ store.chapterCommentTotal }}
              </span>
              <svg class="chapter-comments-arrow" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="m9 18 6-6-6-6" />
              </svg>
            </button>

            <div class="chapter-footer">
              <button class="next-btn" :disabled="!store.hasNext" @click="nextChapter">
                {{ store.hasNext ? '下一章' : '没有更多了' }}
              </button>
            </div>
          </template>
        </div>
      </article>

      <Transition name="fade">
        <div v-if="!store.loading && isHorizontalPageMode && isHorizontalAtEnd" class="horizontal-next-floating">
          <button class="next-btn" :disabled="!store.hasNext" @click="nextChapter">
            {{ store.hasNext ? '下一章' : '没有更多了' }}
          </button>
        </div>
      </Transition>

      <div
        v-if="!store.loading && isContinuousMode"
        class="continuous-reading"
        :style="{
          maxWidth: config.pageWidth + 'px',
          fontSize: config.fontSize + 'px',
          fontWeight: config.fontWeight,
          lineHeight: config.lineHeight,
        }"
      >
        <section
          v-for="chapter in continuousChapters"
          :key="chapter.index"
          class="chapter-content continuous-chapter"
          :data-chapter-index="chapter.index"
        >
          <div class="chapter-title">{{ chapter.title }}</div>

          <div
            class="chapter-text"
            data-role="continuous"
            :data-chapter-index="chapter.index"
            :style="{
              '--p-spacing': config.paragraphSpacing + 'em',
            }"
            v-html="chapter.html"
            @click="handleChapterTextClick"
          ></div>

          <div v-if="chapter.index === continuousChapters[continuousChapters.length - 1]?.index" class="chapter-footer">
            <button class="next-btn" :disabled="!store.hasNext" @click="nextChapter">
              {{ store.hasNext ? '继续加载下一章' : '已经到底了' }}
            </button>
          </div>
        </section>

        <div v-if="continuousLoadingNext" class="continuous-loading-inline">正在加载下一章...</div>
      </div>
    </div>



    <ReaderSearchPanel
      :show="showSearch"
      :theme="chromeTheme"
      :query="searchQuery"
      :results="searchResults"
      :active-index="searchIndex"
      :count="searchCount"
      :status="bookSearchStatus"
      @close="closeSearch"
      @search="runSearch"
      @next="nextSearchResult"
      @prev="prevSearchResult"
      @update:query="searchQuery = $event"
      @jump="jumpToSearchResult"
    />

    <Transition name="fade">
      <div
        v-if="selectionMenu.visible"
        class="selection-menu"
        :class="{ 'as-context-menu': selectionMenu.variant === 'context' }"
        @click.stop
        :style="{
          top: selectionMenu.top + 'px',
          left: selectionMenu.left + 'px',
          background: chromeTheme.popup,
          color: chromeTheme.fontColor,
        }"
      >
        <div class="selection-menu-text">{{ selectionMenu.text }}</div>
        <div class="selection-menu-actions">
          <button @click="addSelectionBookmark">加入书签</button>
          <button @click="addSelectionReplaceRule('book')">按本书替换</button>
          <button @click="addSelectionReplaceRule('source')">按书源替换</button>
        </div>
      </div>
    </Transition>

    <BookDetailModal
      v-model="showBookInfo"
      :book="bookInfoBook"
    />

    <ImageLightbox :src="previewImage" @close="previewImage = ''" />
  </div>
</template>

<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted, nextTick, defineAsyncComponent } from 'vue'
import { onBeforeRouteLeave, useRouter } from 'vue-router'
import { useReaderStore, fontPresets } from '../stores/reader'
import { useAppStore } from '../stores/app'
import { getBookInfo, withAuthQuery } from '../api/bookshelf'
import { applySystemTheme } from '../utils/systemUi'
import { countBrowserBookCache } from '../utils/browserCache'
import { APP_VIEWPORT_CHANGE_EVENT, syncViewportSize } from '../utils/viewport'
import { isReaderInteractiveClickTarget } from '../utils/readerClick'
import { sanitizeUntrustedHtml } from '../utils/sanitize'
import { safeLocalSet } from '../utils/storage'
import type { ChapterImage, ParaReviewCount } from '../types'
import { createReaderProgressAutoSaveScheduler, createReaderProgressExitSaver } from '../utils/readerProgressAutoSave'
import type { Book } from '../types'

import ReaderSidebar from '../components/reader/ReaderSidebar.vue'
import ReaderToolbar from '../components/reader/ReaderToolbar.vue'
import ReaderMobileControls from '../components/reader/ReaderMobileControls.vue'
import { useReaderSearch } from '../composables/useReaderSearch'
import { useReaderSelection } from '../composables/useReaderSelection'
import { useHorizontalPaging } from '../composables/useHorizontalPaging'
import { useContinuousReading } from '../composables/useContinuousReading'
import { useReaderAutoPlayback } from '../composables/useReaderAutoPlayback'

const ReaderCatalog = defineAsyncComponent(() => import('../components/reader/ReaderCatalog.vue'))
const ReadSettings = defineAsyncComponent(() => import('../components/reader/ReadSettings.vue'))
const ReaderBookshelf = defineAsyncComponent(() => import('../components/reader/ReaderBookshelf.vue'))
const ReaderSource = defineAsyncComponent(() => import('../components/reader/ReaderSource.vue'))
const ReplaceRuleManager = defineAsyncComponent(() => import('../components/reader/ReplaceRuleManager.vue'))
const CacheManager = defineAsyncComponent(() => import('../components/reader/CacheManager.vue'))
const BookDetailModal = defineAsyncComponent(() => import('../components/BookDetailModal.vue'))
const ReaderTtsPanel = defineAsyncComponent(() => import('../components/reader/ReaderTtsPanel.vue'))
const ReaderSearchPanel = defineAsyncComponent(() => import('../components/reader/ReaderSearchPanel.vue'))
const CommentPanel = defineAsyncComponent(() => import('../components/reader/CommentPanel.vue'))
const ImageLightbox = defineAsyncComponent(() => import('../components/reader/ImageLightbox.vue'))

const router = useRouter()
const store = useReaderStore()
const appStore = useAppStore()
const READER_POSITION_PREFIX = 'reader-position:'
const SERVER_PROGRESS_AUTOSAVE_MS = 10000

interface SavedReadingPosition {
  chapterIndex: number
  progress: number
  paragraphIndex?: number
  paragraphProgress?: number
  updatedAt: number
}

const CONTINUOUS_POSITION_ANCHOR_RATIO = 0.12

// 排查阅读位置恢复问题时临时打开
const POSITION_DEBUG = false

function debugPositionLog(message: string, payload?: unknown) {
  if (!POSITION_DEBUG) return
  console.debug(`[reader-position] ${message}`, payload ?? '')
}

const config = computed(() => store.config)
const theme = computed(() => store.currentTheme)
const chromeTheme = computed(() => {
  if (store.isNight || appStore.theme === 'dark') {
    return {
      ...store.currentTheme,
      popup: 'var(--color-bg-elevated)',
      fontColor: 'var(--color-text)',
    }
  }
  return store.currentTheme
})

const scrollContainerRef = ref<HTMLElement>()
const chapterTextRef = ref<HTMLElement>()
const showControls = ref(false)
const isMobile = ref(false)
let speechTimerTicker: number | null = null
let suppressNextTapUntil = 0
let restorePositionTimer: number | null = null
let persistPositionTimer: number | null = null
const pendingRestorePosition = ref<SavedReadingPosition | null>(null)
let pendingRestoreAttempts = 0
let suppressPositionSaveUntil = 0
let suppressContinuousScrollSyncUntil = 0
let suppressContinuousAutoLoadUntil = 0
const restoreStabilizeTimers: number[] = []
const serverProgressAutoSaveScheduler = createReaderProgressAutoSaveScheduler({
  intervalMs: SERVER_PROGRESS_AUTOSAVE_MS,
  flush: () => store.flushProgressToServer(),
})
const readerProgressExitSaver = createReaderProgressExitSaver({
  disposeAutoSave: () => serverProgressAutoSaveScheduler.dispose(),
  savePosition: () => saveReadingPosition({ force: true }),
  flushToServer: () => store.flushProgressToServer(true),
  flushToServerKeepalive: () => store.flushProgressToServerKeepalive(true),
})
const isContinuousMode = computed(() =>
  config.value.readMethod === '上下滚动' || config.value.readMethod === '上下滚动2',
)
const hideReadChaptersMode = computed(() => config.value.readMethod === '上下滚动2')
const isHorizontalPageMode = computed(() => config.value.readMethod === '左右翻页')
const isIosWebkit = computed(() => {
  const ua = typeof navigator !== 'undefined' ? navigator.userAgent : ''
  return /iPhone|iPad|iPod/i.test(ua) || (/Macintosh/i.test(ua) && typeof navigator !== 'undefined' && navigator.maxTouchPoints > 1)
})
const disableSystemCallout = computed(() => {
  return isIosWebkit.value && isMobile.value && config.value.selectAction === 'popup'
})
const touchState = ref({
  startX: 0,
  startY: 0,
  startAt: 0,
  moving: false,
  horizontalLocked: false,
})
const showBookInfo = ref(false)
const bookInfoBook = ref<Book | null>(null)
const showTTSPanel = ref(false)
const ttsPanelDismissed = ref(false)
const offlineCachedCount = ref(0)
const speechTimerNow = ref(Date.now())
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
const {
  showSearch,
  searchQuery,
  searchResults,
  searchIndex,
  searchCount,
  bookSearchStatus,
  toggleSearch,
  openSearch,
  closeSearch,
  runSearch,
  nextSearchResult,
  prevSearchResult,
  jumpToSearchResult,
  handleContentUpdated,
  handlePresentationUpdated,
} = useReaderSearch(store)
const {
  selectionMenu,
  suppressSelectionCloseUntil,
  hideSelectionMenu,
  scheduleSelectionMenuUpdate,
  showSelectionMenuAt,
  handleMouseUpSelection,
  handlePressStartSelection,
  handleTouchEndSelection,
  handleSelectionChange,
  addSelectionBookmark,
  addSelectionReplaceRule,
  clearSelectionState,
  disposeSelection,
} = useReaderSelection(
  store,
  appStore,
  computed(() => ({ selectAction: config.value.selectAction })),
  scrollContainerRef,
)

const offlineBannerText = computed(() => {
  if (appStore.isOnline) return ''
  if (offlineCachedCount.value > 0) {
    return `离线模式：当前书已缓存 ${offlineCachedCount.value} 章，可继续阅读已缓存章节`
  }
  return '离线模式：当前书尚未缓存到浏览器，未缓存章节将无法打开'
})

async function refreshOfflineCacheState() {
  if (!store.book) {
    offlineCachedCount.value = 0
    return
  }
  offlineCachedCount.value = await countBrowserBookCache(store.book.bookUrl).catch(() => 0)
}

let refreshOfflineCacheStateTimer: number | null = null

function scheduleRefreshOfflineCacheState() {
  if (refreshOfflineCacheStateTimer) clearTimeout(refreshOfflineCacheStateTimer)
  refreshOfflineCacheStateTimer = window.setTimeout(() => {
    void refreshOfflineCacheState()
  }, 120)
}

// resize 与 viewport 事件双通道都会调 checkMedia：合并到同一帧，
// 否则拖拽窗口/键盘弹起会连续多次全量分页
let checkMediaQueued = false
function checkMedia() {
  isMobile.value = window.innerWidth <= 767
  if (checkMediaQueued) return
  checkMediaQueued = true
  window.setTimeout(() => {
    checkMediaQueued = false
    updateHorizontalMetrics()
    if (isHorizontalPageMode.value) {
      scheduleRebuildHorizontalPages()
    }
  }, 0)
}

// 切章时 currentIndex 与 content 两个 watch 会在同一次 flush 内先后触发：
// 合并到同一计时器，分页测量（每步强制同步布局）只跑一次
let rebuildHorizontalTimer: number | null = null
function scheduleRebuildHorizontalPages() {
  if (rebuildHorizontalTimer) clearTimeout(rebuildHorizontalTimer)
  rebuildHorizontalTimer = window.setTimeout(() => {
    rebuildHorizontalTimer = null
    if (!isHorizontalPageMode.value) return
    rebuildHorizontalPages()
    updateHorizontalEndState()
  }, 0)
}

function handleViewportChange() {
  syncViewportSize()
  checkMedia()
  scheduleRestoreReadingPosition()
}

const currentFontFamily = computed(() => {
  const preset = fontPresets.find(p => p.value === config.value.fontFamily)
  return preset ? preset.family : ''
})

// 基础渲染（消毒 + 段落样式，不含段评气泡）：按正文与排版配置缓存。
// 此前段评索引异步到达、开关搜索等任一变化都会让整章重新走
// sanitize + 逐段 DOM 重建；现在这些只触发便宜的装饰层。
function buildBaseChapterHtml(rawText: string) {
  if (!rawText) return ''
  const text = rawText
  const stripLeadingIndent = (line: string) => line.replace(/^[\u3000\u00A0 \t]+/, '')
  const wrapper = document.createElement('div')

  if (/<[a-z][\s\S]*>/i.test(text)) {
    // 书源返回的正文 HTML 完全不可信：先白名单消毒再进 DOM
    wrapper.innerHTML = sanitizeUntrustedHtml(text)
    if (!config.value.showChapterImages) {
      wrapper.querySelectorAll('img').forEach((image) => image.remove())
    }
    const paragraphs = Array.from(wrapper.querySelectorAll('p')) as HTMLParagraphElement[]
    if (paragraphs.length) {
      paragraphs.forEach((paragraph) => {
        const plainText = (paragraph.textContent || '').replace(/^[\u3000\u00A0 \t]+/, '').trim()
        const hasRenderableChildren = Boolean(paragraph.querySelector('img, br, ruby, table, ul, ol'))
        if (!plainText && !hasRenderableChildren) {
          paragraph.remove()
          return
        }
        paragraph.innerHTML = paragraph.innerHTML.replace(/^[\u3000\u00A0 \t]+/, '')
        paragraph.style.marginTop = '0'
        paragraph.style.marginBottom = `${config.value.paragraphSpacing}em`
        // 图独占一段时不缩进：首行缩进会把整张图往右推
        const imageOnly = Boolean(paragraph.querySelector('img')) && !plainText
        paragraph.classList.toggle('reader-indent', config.value.firstLineIndent && !imageOnly)
      })
    }
  } else {
    wrapper.innerHTML = text
      .split(/\n/)
      .filter((line: string) => line.trim())
      .map((line: string) => {
        const shouldIndent = config.value.firstLineIndent
        const content = escapeHtmlText(stripLeadingIndent(line.trimEnd()))
        return `<p${shouldIndent ? ' class="reader-indent"' : ''} style="margin-top: 0; margin-bottom: ${config.value.paragraphSpacing}em;">${content}</p>`
      })
      .join('')
  }
  return wrapper.innerHTML
}

const baseChapterHtml = computed(() => buildBaseChapterHtml(store.displayContent || ''))

// 装饰层：章节配图 + 段评气泡 + 本地 EPUB 资源改写 + 搜索高亮，作用在基础 HTML 之上
function decorateChapterHtml(baseHtml: string, withBubbles: boolean) {
  const wrapper = document.createElement('div')
  wrapper.innerHTML = baseHtml
  if (withBubbles) {
    // 配图先插：段评气泡按 `<p>` 顺序定位，figure 不是 <p> 不影响它，
    // 但气泡会往段落里塞计数，得赶在它之前比对段落原文
    insertChapterImages(wrapper)
    const bubbleMap = paraBubbleMap.value
    if (bubbleMap.size) {
      // base 渲染时空段已被移除，剩余 <p> 的顺序即段落位置
      let paragraphPosition = 0
      wrapper.querySelectorAll('p').forEach((paragraph) => {
        const bubble = bubbleMap.get(paragraphPosition)
        paragraphPosition += 1
        if (bubble) paragraph.insertAdjacentHTML('beforeend', renderParaCommentBubble(bubble))
      })
    }
  }
  appendLocalEpubAssetAuth(wrapper)
  highlightSearchText(wrapper)
  return wrapper.innerHTML
}

/**
 * 把配图插进正文。
 *
 * 位置是「插在该段之前」；位置缺失或超出正文范围的一律排在章末。
 * 说明文字只在正文里没有同一句话时才显示——番茄把说明也写进了正文，
 * 再显示一遍就是重复。
 */
function insertChapterImages(wrapper: HTMLElement, imageMap = chapterImageMap.value) {
  if (!imageMap.size) return
  const paragraphs = Array.from(wrapper.querySelectorAll('p')) as HTMLElement[]
  const trailing: ChapterImage[] = []
  imageMap.forEach((images, position) => {
    if (position < 0 || position >= paragraphs.length) {
      trailing.push(...images)
      return
    }
    const paragraph = paragraphs[position]
    const paragraphText = (paragraph.textContent || '').trim()
    paragraph.insertAdjacentHTML(
      'beforebegin',
      renderChapterImageFigures(images, paragraphText),
    )
  })
  if (trailing.length) {
    wrapper.insertAdjacentHTML('beforeend', renderChapterImageFigures(trailing, ''))
  }
}

function renderChapterImageFigures(images: ChapterImage[], paragraphText: string) {
  return images
    .map((image) => {
      const caption = image.caption.trim()
      const size = image.width > 0 && image.height > 0 ? ` width="${image.width}" height="${image.height}"` : ''
      const captionHtml =
        caption && caption !== paragraphText
          ? `<figcaption>${escapeHtmlText(caption)}</figcaption>`
          : ''
      // 后端把配图收口成 /reader3/image/<id>：<img> 带不了请求头，令牌只能进查询串
      const src = withAuthQuery(image.url)
      return (
        '<figure class="chapter-figure">' +
        `<img src="${escapeHtmlAttr(src)}" alt="${escapeHtmlAttr(caption)}"` +
        `${size} loading="lazy" referrerpolicy="no-referrer">` +
        `${captionHtml}</figure>`
      )
    })
    .join('')
}

function formatChapterHtml(rawText: string, withParaComments = false) {
  const base = withParaComments ? baseChapterHtml.value : buildBaseChapterHtml(rawText)
  if (!base) return ''
  return decorateChapterHtml(base, withParaComments)
}

/**
 * 渲染段落位置 → 正文原始行号。
 *
 * 后端给的段号是「正文按 `\n` 切分后的下标」，而渲染用的正文已经过书源替换
 * 规则与繁简转换，空行也被丢掉，段号可能对不上。这里以「非空行位置对齐」
 * 为主、段落原文匹配为辅，算出每个渲染段落对应正文里的哪一段。
 * 段评气泡与章节配图都靠它定位。
 */
const rawLineByPosition = computed(() => {
  const map = new Map<number, number>()
  const rawLines = splitParagraphLines(store.content)
  const displayLines = splitParagraphLines(store.displayContent)
  if (!displayLines.length) return map

  if (rawLines.length === displayLines.length) {
    displayLines.forEach((_line, position) => map.set(position, rawLines[position].index))
    return map
  }

  // 行数对不上（替换规则增删了行）：退化成按段落原文匹配。
  // 给原始行建「前缀 → 行号」索引（每行最多 30 个前缀条目），把逐行
  // find 的 O(n²) 降为 O(n·30)；候选仍按原始行顺序取首个未命中项，
  // 匹配语义与原先完全一致
  const prefixIndex = new Map<string, number[]>()
  rawLines.forEach((raw) => {
    const limit = Math.min(30, raw.text.length)
    for (let i = 1; i <= limit; i += 1) {
      const prefix = raw.text.slice(0, i)
      const bucket = prefixIndex.get(prefix)
      if (bucket) bucket.push(raw.index)
      else prefixIndex.set(prefix, [raw.index])
    }
  })
  const used = new Set<number>()
  displayLines.forEach((line, position) => {
    const head = line.text.slice(0, 30)
    if (!head) return
    const hit = prefixIndex.get(head)?.find((idx) => !used.has(idx))
    if (hit === undefined) return
    used.add(hit)
    map.set(position, hit)
  })
  return map
})

const paraBubbleMap = computed(() => {
  const map = new Map<number, ParaReviewCount>()
  const counts = store.paraReviewCountByIndex
  if (!counts.size) return map
  rawLineByPosition.value.forEach((rawIndex, position) => {
    const item = counts.get(rawIndex)
    if (item) map.set(position, item)
  })
  return map
})

/**
 * 正文原始行号 → 渲染段落位置（章末用 `-1`）。
 *
 * 行号落在空行上时，退到它之后第一个非空段落；超出正文则算章末——
 * 配图规则给的插入位置是「插在该行之前」，指向末尾就等于排在章末。
 */
function positionForRawLine(rawLine: number) {
  let position = -1
  let nearest = Number.POSITIVE_INFINITY
  rawLineByPosition.value.forEach((rawIndex, renderedPosition) => {
    if (rawIndex >= rawLine && rawIndex < nearest) {
      nearest = rawIndex
      position = renderedPosition
    }
  })
  return position
}

/** 渲染段落位置（`-1` = 章末）→ 该处要显示的配图。 */
const chapterImageMap = computed(() => {
  const map = new Map<number, ChapterImage[]>()
  if (!store.chapterImagesEnabled || !config.value.showChapterImages) return map
  for (const image of store.chapterImages) {
    const position = image.paraIndex == null ? -1 : positionForRawLine(image.paraIndex)
    const bucket = map.get(position)
    if (bucket) bucket.push(image)
    else map.set(position, [image])
  }
  return map
})

/** 按 `\n` 切分正文，返回非空行的「原始行号 + 文本」。 */
function splitParagraphLines(text: string) {
  const lines: { index: number; text: string }[] = []
  if (!text) return lines
  text.split(/\n/).forEach((line, index) => {
    const trimmed = line.replace(/^[\u3000\u00A0 \t]+/, '').trim()
    if (trimmed) lines.push({ index, text: trimmed })
  })
  return lines
}

function renderParaCommentBubble(item: ParaReviewCount) {
  const count = item.count > 999 ? '999+' : String(item.count)
  // 作者评论/点赞过的段落：气泡加底色，计数后带一支羽毛笔标记（参照番茄的样式）
  const authorMark = item.authorCommented
    ? '<svg class="para-author-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">' +
      '<path d="M20.24 12.24a6 6 0 0 0-8.49-8.49L5 10.5V19h8.5z" />' +
      '<path d="m16 8-14 14" />' +
      '<path d="M17.5 15H9" />' +
      '</svg>'
    : ''
  const title = item.authorCommented ? ' title="作者评论过这一段"' : ''
  return (
    `<span class="para-comment-bubble${item.authorCommented ? ' has-author' : ''}" role="button" tabindex="0" data-para-index="${item.paraIndex}"${title}>` +
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">' +
    '<path d="M21 11.5a8.5 8.5 0 0 1-12.3 7.6L3 21l1.9-5.6A8.5 8.5 0 1 1 21 11.5z" />' +
    '</svg>' +
    `<span class="para-comment-count">${count}</span>` +
    authorMark +
    '</span>'
  )
}

function handleChapterTextClick(event: MouseEvent) {
  const target = event.target as HTMLElement | null
  const bubble = target?.closest('.para-comment-bubble') as HTMLElement | null
  if (bubble) {
    event.stopPropagation()
    const paraIndex = Number(bubble.dataset.paraIndex)
    if (!Number.isFinite(paraIndex)) return
    openParaComments(paraIndex)
    return
  }
  // 正文配图：点开看大图
  if (target?.tagName === 'IMG') {
    event.stopPropagation()
    previewImage.value = (target as HTMLImageElement).src
  }
}

/* ─── 图片放大预览 ─── */
const previewImage = ref('')

/* ─── 评论面板 ─── */
const showCommentPanel = ref(false)
const commentMode = ref<'chapter' | 'para'>('chapter')
const commentParaIndex = ref(0)
const commentParaText = computed(
  () => store.paraReviewCountByIndex.get(commentParaIndex.value)?.text || '',
)
// 概览没标记 authorCommented 的段落，作者扫描注定空手，「只看作者」入口不显示
const commentParaAuthorMarked = computed(
  () => store.paraReviewCountByIndex.get(commentParaIndex.value)?.authorCommented === true,
)

function openChapterComments() {
  commentMode.value = 'chapter'
  showCommentPanel.value = true
}

function openParaComments(paraIndex: number) {
  commentMode.value = 'para'
  commentParaIndex.value = paraIndex
  showCommentPanel.value = true
}

function closeCommentPanel() {
  showCommentPanel.value = false
}

function appendLocalEpubAssetAuth(root: HTMLElement) {
  const images = Array.from(root.querySelectorAll('img')) as HTMLImageElement[]
  images.forEach((image) => {
    const src = image.getAttribute('src') || ''
    if (src.startsWith('/reader3/localEpubAsset')) {
      image.setAttribute('src', withAuthQuery(src))
    }
  })
}

function highlightSearchText(root: HTMLElement) {
  if (!showSearch.value || !searchQuery.value) return
  const regex = new RegExp(escapeRegExp(searchQuery.value), 'gi')
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement
      if (!parent || ['MARK', 'SCRIPT', 'STYLE'].includes(parent.tagName)) {
        return NodeFilter.FILTER_REJECT
      }
      regex.lastIndex = 0
      return regex.test(node.textContent || '') ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT
    },
  })
  const nodes: Text[] = []
  while (walker.nextNode()) {
    nodes.push(walker.currentNode as Text)
  }
  nodes.forEach((node) => {
    const value = node.textContent || ''
    regex.lastIndex = 0
    const fragment = document.createDocumentFragment()
    let cursor = 0
    for (const match of value.matchAll(regex)) {
      const index = match.index ?? 0
      if (index > cursor) {
        fragment.appendChild(document.createTextNode(value.slice(cursor, index)))
      }
      const mark = document.createElement('mark')
      mark.className = 'search-highlight'
      mark.textContent = match[0]
      fragment.appendChild(mark)
      cursor = index + match[0].length
    }
    if (cursor < value.length) {
      fragment.appendChild(document.createTextNode(value.slice(cursor)))
    }
    node.parentNode?.replaceChild(fragment, node)
  })
}

function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function escapeHtmlText(value: string) {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
}

/** 属性值转义：除了 `&<>` 还要挡引号，否则能拼出新的属性。 */
function escapeHtmlAttr(value: string) {
  return escapeHtmlText(value).replace(/"/g, '&quot;').replace(/'/g, '&#39;')
}

/**
 * 渲染某一章的正文 HTML。
 *
 * `chapterIndex` 用于章节配图：配图是「当前章」的状态（后端按章返回），
 * 连续滚动模式下只有当前章该插配图，前后预加载的章节不插。
 */
function renderChapterHtml(rawText: string, chapterIndex?: number) {
  const html = formatChapterHtml(store.processContentForDisplay(rawText || ''))
  if (chapterIndex === undefined) return html
  const images = chapterIndex === store.currentIndex ? chapterImageMap.value : null
  if (!images?.size) return html
  const wrapper = document.createElement('div')
  wrapper.innerHTML = html
  insertChapterImages(wrapper, images)
  return wrapper.innerHTML
}

const formattedContent = computed(() => formatChapterHtml(store.displayContent || '', true))

const {
  horizontalPageIndex,
  horizontalPageStep,
  horizontalPageStepStyle,
  horizontalPages,
  isHorizontalAtEnd,
  rebuildHorizontalPages,
  updateHorizontalMetrics,
  updateHorizontalEndState,
  alignHorizontalToNearestPage,
  resetHorizontalPagePosition,
} = useHorizontalPaging(
  store,
  computed(() => ({
    fontSize: config.value.fontSize,
    fontWeight: config.value.fontWeight,
    lineHeight: config.value.lineHeight,
  })),
  currentFontFamily,
  formattedContent,
  isHorizontalPageMode,
  scrollContainerRef,
)

const horizontalPageTransform = computed(() => {
  const offset = horizontalPageIndex.value * Math.max(1, horizontalPageStep.value)
  return `translate3d(${-offset}px, 0, 0)`
})
const horizontalPageTransitionDuration = computed(() => {
  const duration = Number(config.value.animateDuration) || 0
  if (duration <= 0) return '0ms'
  return `${Math.min(220, duration)}ms`
})
const {
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
  clearContinuousChapters,
  disposeContinuousReading,
} = useContinuousReading(
  store,
  renderChapterHtml,
  isContinuousMode,
  hideReadChaptersMode,
  scrollContainerRef,
)

function syncHorizontalPageState() {
  const maxPage = Math.max(0, horizontalPages.value.length - 1)
  const progress = maxPage <= 0 ? 1 : horizontalPageIndex.value / maxPage
  store.setChapterScrollProgress(progress)
  updateHorizontalEndState()
  if (config.value.enablePreload && maxPage > 0 && horizontalPageIndex.value >= maxPage - 1) {
    store.preloadAroundChapter(store.currentIndex)
  }
  scheduleSaveReadingPosition()
  serverProgressAutoSaveScheduler.schedule()
}

function pageForward() {
  const container = scrollContainerRef.value
  if (!container) return
  if (isHorizontalPageMode.value) {
    const maxPage = Math.max(0, horizontalPages.value.length - 1)
    if (horizontalPageIndex.value >= maxPage) {
      nextChapter()
      return
    }
    horizontalPageIndex.value = Math.min(maxPage, horizontalPageIndex.value + 1)
    container.scrollTo({ left: 0, behavior: 'auto' })
    syncHorizontalPageState()
    return
  }
  const step = container.clientHeight * 0.88
  if (container.scrollTop + container.clientHeight >= container.scrollHeight - 10) {
    nextChapter()
    return
  }
  container.scrollBy({ top: step, behavior: 'smooth' })
}

function pageBackward() {
  const container = scrollContainerRef.value
  if (!container) return
  if (isHorizontalPageMode.value) {
    if (horizontalPageIndex.value <= 0) {
      prevChapter()
      return
    }
    horizontalPageIndex.value = Math.max(0, horizontalPageIndex.value - 1)
    container.scrollTo({ left: 0, behavior: 'auto' })
    syncHorizontalPageState()
    return
  }
  const step = container.clientHeight * 0.88
  if (container.scrollTop <= 10) {
    prevChapter()
    return
  }
  container.scrollBy({ top: -step, behavior: 'smooth' })
}

// Navigation
async function goBack() {
  await persistReadingProgressBeforeLeave()
  // 从发现/最近/搜索进入时返回来处；直接落在阅读器（刷新、直链）时兜底回首页
  if (window.history.state?.back != null) {
    router.back()
  } else {
    router.replace('/')
  }
}

function handlePageHide() {
  store.flushReaderSessionSave()
  persistReadingProgressKeepalive()
}

function handleBeforeUnload() {
  store.flushReaderSessionSave()
  persistReadingProgressKeepalive()
}

function handleVisibilityChange() {
  if (document.visibilityState !== 'hidden') return
  persistReadingProgressTemporaryKeepalive()
}

async function persistReadingProgressBeforeLeave() {
  await readerProgressExitSaver.flushBeforeRouteLeave()
}

function persistReadingProgressKeepalive() {
  readerProgressExitSaver.flushKeepalive()
}

function persistReadingProgressTemporaryKeepalive() {
  readerProgressExitSaver.flushTemporaryKeepalive()
}

async function prevChapter() {
  const targetIndex = store.currentIndex - 1
  if (targetIndex < 0) return

  if (!isContinuousMode.value) {
    await store.prevChapter()
    scrollToTop()
    return
  }

  await rebuildContinuousAtChapter(targetIndex)
}

async function nextChapter() {
  const targetIndex = store.currentIndex + 1
  if (targetIndex >= store.chapters.length) return

  if (!isContinuousMode.value) {
    await store.nextChapter()
    scrollToTop()
    return
  }

  await rebuildContinuousAtChapter(targetIndex)
}

async function jumpFromCatalog(targetIndex: number) {
  if (targetIndex < 0 || targetIndex >= store.chapters.length) return

  if (!isContinuousMode.value) {
    await store.loadChapter(targetIndex)
    store.closePanel()
    scrollToTop()
    return
  }

  await rebuildContinuousAtChapter(targetIndex)
  store.closePanel()
}

async function rebuildContinuousAtChapter(targetIndex: number) {
  suppressContinuousScrollSyncUntil = Date.now() + 500
  suppressContinuousAutoLoadUntil = Date.now() + 500
  await initializeContinuousChapters(targetIndex, false)
}

function scrollToTop() {
  if (scrollContainerRef.value) {
    if (isHorizontalPageMode.value) {
      scrollContainerRef.value.scrollTo({ left: 0, behavior: 'smooth' })
    } else {
      scrollContainerRef.value.scrollTo({ top: 0, behavior: 'smooth' })
    }
  }
}

function scrollToBottom() {
  if (scrollContainerRef.value) {
    scrollContainerRef.value.scrollTo({ top: scrollContainerRef.value.scrollHeight, behavior: 'smooth' })
  }
}

function getPositionStorageKey() {
  return store.book?.bookUrl ? `${READER_POSITION_PREFIX}${store.book.bookUrl}` : ''
}

function normalizePositionTimestamp(value?: number | null) {
  if (typeof value !== 'number' || Number.isNaN(value) || value <= 0) return 0
  return value < 1_000_000_000_000 ? value * 1000 : value
}

function buildServerSavedPosition(): SavedReadingPosition | null {
  if (!store.book) return null
  if (store.book.durChapterIndex !== store.currentIndex) return null
  const rawPos = typeof store.book.durChapterPos === 'number' ? store.book.durChapterPos : 0
  const progress = rawPos > 1 ? rawPos / 10000 : rawPos
  return {
    chapterIndex: store.currentIndex,
    progress: Math.max(0, Math.min(1, progress || 0)),
    updatedAt: normalizePositionTimestamp(store.book.durChapterTime),
  }
}

function loadSavedReadingPosition() {
  const key = getPositionStorageKey()
  if (!key) {
    pendingRestorePosition.value = null
    pendingRestoreAttempts = 0
    debugPositionLog('skip load: no storage key')
    return
  }
  try {
    const raw = localStorage.getItem(key)
    const localSaved = raw ? JSON.parse(raw) as SavedReadingPosition : null
    const serverSaved = buildServerSavedPosition()

    let selected: SavedReadingPosition | null = null
    let source: 'local' | 'server' | 'none' = 'none'

    if (localSaved && localSaved.chapterIndex === store.currentIndex) {
      selected = localSaved
      source = 'local'
    }

    if (serverSaved && serverSaved.chapterIndex === store.currentIndex) {
      if (!selected || normalizePositionTimestamp(serverSaved.updatedAt) > normalizePositionTimestamp(selected.updatedAt)) {
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

    pendingRestorePosition.value = selected
    pendingRestoreAttempts = 0
    clearRestoreStabilizers()
    debugPositionLog('loaded saved position', {
      key,
      saved: selected,
      source,
      localSaved,
      serverSaved,
      currentIndex: store.currentIndex,
      accepted: !!pendingRestorePosition.value,
    })
    if (pendingRestorePosition.value) {
      suppressPositionSaveUntil = Date.now() + 2500
    }
  } catch {
    pendingRestorePosition.value = null
    pendingRestoreAttempts = 0
    clearRestoreStabilizers()
    debugPositionLog('failed to parse saved position', { key })
  }
}

function saveReadingPosition(options: { force?: boolean } = {}) {
  const key = getPositionStorageKey()
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
  const anchorViewportY = container.getBoundingClientRect().top + container.clientHeight * anchorRatio
  if (isContinuousMode.value && continuousChapters.value.length) {
    const section = container.querySelector(`.continuous-chapter[data-chapter-index="${store.currentIndex}"]`) as HTMLElement | null
    const paragraphs = Array.from(section?.querySelectorAll('.chapter-text p') || []) as HTMLElement[]
    if (paragraphs.length) {
      let activeParagraph = paragraphs[0]
      let paragraphIndex = 0
      paragraphs.forEach((paragraph, index) => {
        if (paragraph.getBoundingClientRect().top <= anchorViewportY) {
          activeParagraph = paragraph
          paragraphIndex = index
        }
      })
      const rect = activeParagraph.getBoundingClientRect()
      const paragraphProgress = rect.height > 0 ? Math.max(0, Math.min(1, (anchorViewportY - rect.top) / rect.height)) : 0
      basePosition.paragraphIndex = paragraphIndex
      basePosition.paragraphProgress = paragraphProgress
    }
  } else if (!isHorizontalPageMode.value) {
    const paragraphs = Array.from(chapterTextRef.value?.querySelectorAll('p') || []) as HTMLElement[]
    if (paragraphs.length) {
      let activeParagraph = paragraphs[0]
      let paragraphIndex = 0
      paragraphs.forEach((paragraph, index) => {
        if (paragraph.getBoundingClientRect().top <= anchorViewportY) {
          activeParagraph = paragraph
          paragraphIndex = index
        }
      })
      const rect = activeParagraph.getBoundingClientRect()
      const paragraphProgress = rect.height > 0 ? Math.max(0, Math.min(1, (anchorViewportY - rect.top) / rect.height)) : 0
      basePosition.paragraphIndex = paragraphIndex
      basePosition.paragraphProgress = paragraphProgress
    }
  }

  safeLocalSet(key, JSON.stringify(basePosition))
  debugPositionLog('saved position', { key, position: basePosition })
}

function scheduleSaveReadingPosition() {
  if (persistPositionTimer) clearTimeout(persistPositionTimer)
  persistPositionTimer = window.setTimeout(() => {
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

function scheduleRestoreStabilization(saved: SavedReadingPosition) {
  clearRestoreStabilizers()
  if (!isIosWebkit.value || isHorizontalPageMode.value) return
  ;[140, 320, 680].forEach((delay) => {
    const timer = window.setTimeout(() => {
      if (store.loading || !scrollContainerRef.value || saved.chapterIndex !== store.currentIndex) return
      void nextTick(() => {
        restoreReadingPositionInternal(saved, false)
      })
    }, delay)
    restoreStabilizeTimers.push(timer)
  })
}

function restoreReadingPositionInternal(saved: SavedReadingPosition | null, finalize: boolean) {
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
    if (store.loading || container.scrollWidth <= container.clientWidth + 4) {
      debugPositionLog('restore waiting: horizontal content not ready', {
        saved,
        loading: store.loading,
        scrollWidth: container.scrollWidth,
        clientWidth: container.clientWidth,
      })
      return false
    }
    const maxScroll = Math.max(0, container.scrollWidth - container.clientWidth)
    container.scrollTo({ left: maxScroll * Math.max(0, Math.min(1, saved.progress || 0)), behavior: 'auto' })
    if (finalize) {
      pendingRestorePosition.value = null
      pendingRestoreAttempts = 0
    }
    debugPositionLog('restored horizontal position', { saved, maxScroll })
    return true
  }

  const anchorOffset = container.clientHeight * (isContinuousMode.value ? CONTINUOUS_POSITION_ANCHOR_RATIO : 0.3)
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
    const section = container.querySelector(`.continuous-chapter[data-chapter-index="${saved.chapterIndex}"]`) as HTMLElement | null
    if (!section) {
      debugPositionLog('restore failed: section not found', {
        saved,
        availableSections: Array.from(container.querySelectorAll('.continuous-chapter')).map((el) => (el as HTMLElement).dataset.chapterIndex),
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
      const paragraph = paragraphs[Math.max(0, Math.min(paragraphs.length - 1, saved.paragraphIndex))]
      const top = paragraph.getBoundingClientRect().top - container.getBoundingClientRect().top + container.scrollTop
      const paragraphProgress = Math.max(0, Math.min(1, saved.paragraphProgress || 0))
      targetTop = Math.max(section.offsetTop, top + paragraph.offsetHeight * paragraphProgress - anchorOffset)
    } else {
      const nextSection = section.nextElementSibling as HTMLElement | null
      const sectionHeight = Math.max(1, (nextSection ? nextSection.offsetTop : container.scrollHeight) - section.offsetTop)
      if ((saved.progress || 0) > 0 && sectionHeight <= Math.max(1, container.clientHeight * 0.25)) {
        debugPositionLog('restore waiting: continuous section height not ready', {
          saved,
          sectionHeight,
          clientHeight: container.clientHeight,
        })
        return false
      }
      targetTop = Math.max(
        section.offsetTop,
        section.offsetTop + sectionHeight * Math.max(0, Math.min(1, saved.progress || 0)),
      )
    }
  } else {
    const paragraphs = Array.from(chapterTextRef.value?.querySelectorAll('p') || []) as HTMLElement[]
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
      const paragraph = paragraphs[Math.max(0, Math.min(paragraphs.length - 1, saved.paragraphIndex))]
      const top = paragraph.getBoundingClientRect().top - container.getBoundingClientRect().top + container.scrollTop
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

  container.scrollTo({ top: Math.max(0, targetTop), behavior: 'auto' })
  if (finalize) {
    pendingRestorePosition.value = null
    pendingRestoreAttempts = 0
    const suppressMs = isContinuousMode.value && isIosWebkit.value ? 1600 : 500
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
  restorePositionTimer = window.setTimeout(() => {
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
  }, pendingRestoreAttempts === 0 ? 0 : 80)
}

const {
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
} = useReaderAutoPlayback(
  store,
  computed(() => ({
    autoPageMode: config.value.autoPageMode,
    clickAction: config.value.clickAction,
    scrollPixel: config.value.scrollPixel,
    pageSpeed: config.value.pageSpeed,
    fontSize: config.value.fontSize,
    lineHeight: config.value.lineHeight,
  })),
  isContinuousMode,
  scrollContainerRef,
  chapterTextRef,
  nextChapter,
  prevChapter,
)

// Click behavior
function handleBackgroundClick(e: Event) {
  // If clicked directly on the reader-view wrapper, toggle controls
  if ((e.target as HTMLElement).classList.contains('reader-view')) {
    showControls.value = false
  }
}

function handleContextMenu(event: MouseEvent) {
  // 「右键菜单」模式：选区上右键唤出操作菜单
  if (config.value.selectAction === 'contextmenu') {
    if (showSelectionMenuAt(event)) {
      event.preventDefault()
    }
    return
  }
  if (!disableSystemCallout.value) return
  event.preventDefault()
}

function handleGlobalClick(e: MouseEvent) {
  if (store.activePanel) return
  if (Date.now() < suppressNextTapUntil) return
  if (Date.now() < suppressSelectionCloseUntil.value) return
  if (selectionMenu.value.visible) {
    hideSelectionMenu()
    return
  }
  if (window.getSelection?.()?.toString().trim()) return

  const target = e.target as HTMLElement | null
  if (isReaderInteractiveClickTarget(target)) return
  if (showControls.value && !store.activePanel) {
    showControls.value = false
    return
  }
  if (store.isAutoScrolling) return
  
  if (isHorizontalPageMode.value && isMobile.value) {
    const x = e.clientX / window.innerWidth
    if (x < 0.3) {
      clickZoneAction('prev')
    } else if (x > 0.7) {
      clickZoneAction('next')
    } else {
      clickZoneAction('menu')
    }
  } else {
    const y = e.clientY / window.innerHeight
    if (y < 0.3) {
      clickZoneAction('prev')
    } else if (y > 0.7) {
      clickZoneAction('next')
    } else {
      clickZoneAction('menu')
    }
  }
}

function clickZoneAction(zone: 'prev' | 'menu' | 'next') {
  if (store.isAutoScrolling) return

  if (zone === 'menu') {
    if (isMobile.value) {
      showControls.value = !showControls.value
    }
    return
  }
  
  if (config.value.clickAction === 'none') return
  
  const container = scrollContainerRef.value
  if (!container) return
  
  if (isHorizontalPageMode.value) {
    if (zone === 'next') pageForward()
    else pageBackward()
    return
  }

  const h = container.clientHeight
  const delta = h * 0.8 // Page scroll amount

  if (config.value.clickAction === 'next') {
    pageForward()
    return
  }
  
  if (zone === 'next') {
    if (container.scrollTop + h >= container.scrollHeight - 10) {
      if (config.value.clickAction === 'auto') nextChapter()
    } else {
      container.scrollBy({ top: delta, behavior: 'smooth' })
    }
  } else {
    if (container.scrollTop === 0) {
      if (config.value.clickAction === 'auto') prevChapter()
    } else {
      container.scrollBy({ top: -delta, behavior: 'smooth' })
    }
  }
}

function handleScroll() {
  hideSelectionMenu()
  const container = scrollContainerRef.value
  if (container && isContinuousMode.value && continuousChapters.value.length) {
    if (Date.now() < suppressContinuousScrollSyncUntil) {
      scheduleSaveReadingPosition()
      return
    }
    const sections = getContinuousSections()
    if (sections.length) {
      const anchorLine = container.scrollTop + container.clientHeight * CONTINUOUS_POSITION_ANCHOR_RATIO
      let activeSection = sections[0]
      for (const section of sections) {
        if (section.offsetTop <= anchorLine) {
          activeSection = section
        } else {
          break
        }
      }

      const activeIndex = Number(activeSection.dataset.chapterIndex || 0)
      const activeChapter = getContinuousChapter(activeIndex)
      const nextSection = sections[sections.indexOf(activeSection) + 1] || null
      const sectionRange = Math.max(
        1,
        (nextSection ? nextSection.offsetTop : container.scrollHeight) - activeSection.offsetTop,
      )
      const progress = Math.max(0, Math.min(1, (container.scrollTop - activeSection.offsetTop) / sectionRange))
      if (activeChapter) {
        if (store.currentIndex !== activeIndex || store.content !== activeChapter.content) {
          setContinuousActiveChapter(activeIndex, activeChapter.content, progress)
        } else {
          store.setChapterScrollProgress(progress)
        }
      }
    }

    if (Date.now() >= suppressContinuousAutoLoadUntil && container.scrollHeight - (container.scrollTop + container.clientHeight) < 480) {
      loadContinuousNext()
    }
  } else if (container) {
    const maxScroll = Math.max(1, container.scrollHeight - container.clientHeight)
    const progress = isHorizontalPageMode.value
      ? (() => {
          const maxPage = Math.max(0, horizontalPages.value.length - 1)
          return maxPage <= 0 ? 1 : horizontalPageIndex.value / maxPage
        })()
      : (container.scrollHeight <= container.clientHeight ? 1 : container.scrollTop / maxScroll)
    store.setChapterScrollProgress(progress)
    if (isHorizontalPageMode.value) {
      updateHorizontalMetrics()
      const maxPage = Math.max(0, horizontalPages.value.length - 1)
      horizontalPageIndex.value = Math.max(0, Math.min(maxPage, horizontalPageIndex.value))
      if (container.scrollLeft !== 0) {
        container.scrollTo({ left: 0, behavior: 'auto' })
      }
      updateHorizontalEndState()
      if (config.value.enablePreload && maxPage > 0 && horizontalPageIndex.value >= maxPage - 1) {
        store.preloadAroundChapter(store.currentIndex)
      }
    } else if (config.value.enablePreload && container.scrollHeight - (container.scrollTop + container.clientHeight) < container.clientHeight * 1.5) {
      store.preloadAroundChapter(store.currentIndex)
    }
  }
  if (showControls.value && !store.activePanel) {
    showControls.value = false
  }
  scheduleSaveReadingPosition()
  serverProgressAutoSaveScheduler.schedule()
}

function handleTouchStart(event: TouchEvent) {
  stopAutoScroll()
  hideSelectionMenu()
  const touch = event.touches[0]
  if (!touch) return
  touchState.value = {
    startX: touch.clientX,
    startY: touch.clientY,
    startAt: Date.now(),
    moving: true,
    horizontalLocked: false,
  }
}

function handleTouchMove(event: TouchEvent) {
  if (!isMobile.value || config.value.readMethod !== '左右翻页' || !touchState.value.moving) return
  const selectedText = window.getSelection?.()?.toString().trim()
  if (selectedText) return
  // Keep long-press text selection gestures available on mobile.
  if (Date.now() - touchState.value.startAt > 220) return
  const touch = event.touches[0]
  if (!touch) return
  const deltaX = touch.clientX - touchState.value.startX
  const deltaY = touch.clientY - touchState.value.startY
  if (Math.abs(deltaX) > 12 && Math.abs(deltaX) > Math.abs(deltaY)) {
    touchState.value.horizontalLocked = true
    event.preventDefault()
  }
}

function handleTouchEnd(event: TouchEvent) {
  if (!isMobile.value || config.value.readMethod !== '左右翻页' || !touchState.value.moving) {
    touchState.value.moving = false
    return
  }
  const target = event.target as HTMLElement | null
  if (isReaderInteractiveClickTarget(target)) {
    touchState.value.moving = false
    return
  }
  const touchDuration = Date.now() - touchState.value.startAt
  const selectedText = window.getSelection?.()?.toString().trim()
  if (selectedText) {
    suppressNextTapUntil = Date.now() + 900
    touchState.value.moving = false
    scheduleSelectionMenuUpdate(260)
    return
  }
  const touch = event.changedTouches[0]
  if (!touch) {
    touchState.value.moving = false
    return
  }
  const deltaX = touch.clientX - touchState.value.startX
  const deltaY = touch.clientY - touchState.value.startY
  let didPageTurn = false
  if (Math.abs(deltaX) > 18 && Math.abs(deltaX) > Math.abs(deltaY)) {
    suppressNextTapUntil = Date.now() + 350
    if (deltaX < 0) {
      pageForward()
    } else {
      pageBackward()
    }
    didPageTurn = true
  }
  touchState.value.moving = false
  if (!didPageTurn && touchDuration > 260) {
    // Long-press should be reserved for native text selection, not page action.
    suppressNextTapUntil = Date.now() + 900
    scheduleSelectionMenuUpdate(260)
    return
  }
  if (!didPageTurn) {
    const moved = Math.hypot(deltaX, deltaY)
    if (touchDuration <= 260 && moved < 10) {
      suppressNextTapUntil = Date.now() + 350
      if (showControls.value && !store.activePanel) {
        showControls.value = false
      } else {
        const x = touch.clientX / window.innerWidth
        if (x < 0.3) {
          clickZoneAction('prev')
        } else if (x > 0.7) {
          clickZoneAction('next')
        } else {
          clickZoneAction('menu')
        }
      }
    } else {
      window.setTimeout(() => {
        alignHorizontalToNearestPage(touchState.value.moving)
      }, 120)
    }
  }
  scheduleSelectionMenuUpdate(260)
}

function openCachePanel() {
  store.togglePanel('cache')
}

// Keyboard shortcuts
function handleKeydown(e: KeyboardEvent) {
  const activeElement = document.activeElement as HTMLElement | null
  const tagName = activeElement?.tagName?.toLowerCase()
  if (tagName === 'input' || tagName === 'textarea' || tagName === 'select' || activeElement?.isContentEditable) {
    return
  }

  // Handle Escape key first - close panels or go home
  if (e.key === 'Escape') {
    if (store.activePanel) {
      store.closePanel()
      return
    }
    if (selectionMenu.value.visible) {
      hideSelectionMenu()
      return
    }
    if (showSearch.value) {
      closeSearch()
      return
    }
    if (showTTSPanel.value) {
      closeTTSPanel()
      return
    }
    if (showBookInfo.value) {
      showBookInfo.value = false
      return
    }
    if (showControls.value) {
      showControls.value = false
      return
    }
    // If nothing is open, go back
    goBack()
    return
  }

  // Don't process other keys when panels are open
  if (store.activePanel) return

  const container = scrollContainerRef.value
  if (!container) return

  const h = container.clientHeight

  switch (e.key) {
    case ' ':
    case 'Space':
      e.preventDefault()
      pageForward()
      break
    case 'ArrowDown':
    case 'PageDown':
      e.preventDefault()
      if (isHorizontalPageMode.value) {
        pageForward()
      } else {
        container.scrollBy({ top: h * 0.8, behavior: 'smooth' })
      }
      break
    case 'ArrowUp':
    case 'PageUp':
      e.preventDefault()
      if (isHorizontalPageMode.value) {
        pageBackward()
      } else {
        container.scrollBy({ top: -(h * 0.8), behavior: 'smooth' })
      }
      break
    case 'ArrowRight':
      e.preventDefault()
      nextChapter()
      break
    case 'ArrowLeft':
      e.preventDefault()
      prevChapter()
      break
    case 'Home':
      e.preventDefault()
      scrollToTop()
      break
    case 'End':
      e.preventDefault()
      scrollToBottom()
      break
  }
}

// Toolbar actions
async function toggleBookmark() {
  store.togglePanel('bookmark')
}

function handleTTS() {
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
  if (!store.isSpeaking) {
    startSpeech()
    return
  }
  cancelSpeechTransition()
  store.pauseTTS()
}

function handleStopTTS() {
  cancelSpeechTransition()
  store.stopTTS()
}

watch(() => store.isAutoScrolling, (val) => {
  if (val) startAutoScroll()
  else stopAutoScroll()
})

function changeVoice(name: string) {
  store.setVoiceName(name)
  ttsPanelDismissed.value = false
  showTTSPanel.value = true
  if (store.isSpeaking && !store.isPaused) {
    restartSpeechFromCurrentParagraph()
  }
}

function changeOpenAIVoice(voiceId: string) {
  if (store.speechConfig.openaiSource === 'server') return
  store.setOpenAISpeechVoice(voiceId)
  ttsPanelDismissed.value = false
  showTTSPanel.value = true
  if (store.isSpeaking && !store.isPaused) {
    restartSpeechFromCurrentParagraph()
  }
}

function adjustSpeechRate(delta: number) {
  const next = Math.max(0.5, Math.min(3, parseFloat((store.speechConfig.speechRate + delta).toFixed(1))))
  store.setSpeechRate(next)
  ttsPanelDismissed.value = false
  showTTSPanel.value = true
  if (store.isSpeaking && !store.isPaused) {
    restartSpeechFromCurrentParagraph()
  }
}

function adjustSpeechPitch(delta: number) {
  const next = Math.max(0.5, Math.min(2, parseFloat((store.speechConfig.speechPitch + delta).toFixed(1))))
  store.setSpeechPitch(next)
  ttsPanelDismissed.value = false
  showTTSPanel.value = true
  if (store.isSpeaking && !store.isPaused) {
    restartSpeechFromCurrentParagraph()
  }
}

function setSpeechTimer(minutes: number) {
  store.setSpeechStopTimer(minutes)
  ttsPanelDismissed.value = false
  showTTSPanel.value = true
}
async function openInfo() {
  if (!store.book) return
  showBookInfo.value = true
  bookInfoBook.value = {
    ...store.book,
    durChapterIndex: store.currentIndex,
    durChapterTitle: store.currentChapter?.title || store.book.durChapterTitle,
  }
  try {
    const latest = await getBookInfo(store.book.bookUrl, store.book.origin)
    bookInfoBook.value = {
      ...store.book,
      ...latest,
      durChapterIndex: store.currentIndex,
      durChapterTitle: store.currentChapter?.title || latest.durChapterTitle || store.book.durChapterTitle,
    }
  } catch {
    appStore.showToast('获取书籍详情失败，已显示当前缓存信息', 'warning')
  }
}

function openAiBook() {
  if (!store.book) return
  router.push({
    name: 'ai-book',
    query: { bookUrl: store.book.bookUrl },
  })
}

onBeforeRouteLeave(() => {
  persistReadingProgressKeepalive()
  return true
})

onMounted(async () => {
  syncViewportSize()
  appStore.startReadingSession()
  if (!store.book) {
    const restored = await store.restorePersistedSession()
    if (!restored) {
      router.replace('/')
      return
    }
    appStore.showToast('已恢复最近阅读的离线章节', 'success')
  }
  loadSavedReadingPosition()
  window.addEventListener('keydown', handleKeydown)
  document.addEventListener('mouseup', handleMouseUpSelection)
  document.addEventListener('touchend', handleTouchEndSelection)
  document.addEventListener('mousedown', handlePressStartSelection)
  document.addEventListener('touchstart', handlePressStartSelection)
    document.addEventListener('selectionchange', handleSelectionChange)
    checkMedia()
    window.addEventListener('resize', checkMedia)
    window.addEventListener(APP_VIEWPORT_CHANGE_EVENT, handleViewportChange)
    window.addEventListener('pagehide', handlePageHide)
    window.addEventListener('beforeunload', handleBeforeUnload)
    document.addEventListener('visibilitychange', handleVisibilityChange)
    store.fetchVoices()
  applySystemTheme(store.isNight ? 'dark' : appStore.theme, store.currentTheme.body)
  if (typeof window !== 'undefined' && window.speechSynthesis) {
    window.speechSynthesis.onvoiceschanged = () => store.fetchVoices()
  }
  speechTimerTicker = window.setInterval(() => {
    speechTimerNow.value = Date.now()
  }, 15000)
  await Promise.all([
    store.fetchBookmarks(),
    store.fetchReplaceRules(),
  ])
  scheduleRefreshOfflineCacheState()
  updateHorizontalMetrics()
  await rebuildHorizontalPages()
  if (isContinuousMode.value) {
    await initializeContinuousChapters(store.currentIndex, false)
  }
  scheduleRestoreReadingPosition()
})

onUnmounted(() => {
  store.flushReaderSessionSave()
    persistReadingProgressKeepalive()
    appStore.stopReadingSession()
    window.removeEventListener('keydown', handleKeydown)
  document.removeEventListener('mouseup', handleMouseUpSelection)
  document.removeEventListener('touchend', handleTouchEndSelection)
  document.removeEventListener('mousedown', handlePressStartSelection)
  document.removeEventListener('touchstart', handlePressStartSelection)
    document.removeEventListener('selectionchange', handleSelectionChange)
    window.removeEventListener('resize', checkMedia)
    window.removeEventListener(APP_VIEWPORT_CHANGE_EVENT, handleViewportChange)
    window.removeEventListener('pagehide', handlePageHide)
    window.removeEventListener('beforeunload', handleBeforeUnload)
    document.removeEventListener('visibilitychange', handleVisibilityChange)
  if (speechTimerTicker) clearInterval(speechTimerTicker)
  if (restorePositionTimer) clearTimeout(restorePositionTimer)
  if (persistPositionTimer) clearTimeout(persistPositionTimer)
  if (refreshOfflineCacheStateTimer) clearTimeout(refreshOfflineCacheStateTimer)
  if (rebuildHorizontalTimer) clearTimeout(rebuildHorizontalTimer)
  clearRestoreStabilizers()
  disposeSelection()
  disposeContinuousReading()
  disposeAutoPlayback()
  store.stopTTS()
  if (typeof window !== 'undefined' && window.speechSynthesis) {
    window.speechSynthesis.onvoiceschanged = null
  }
  applySystemTheme(appStore.theme)
  store.closePanel()
})

watch(() => config.value.autoPageMode, () => {
  if (!store.isAutoScrolling) return
  stopAutoScroll()
  store.isAutoScrolling = true
  startAutoScroll()
})

watch(() => config.value.readMethod, async () => {
  clearSelectionState()
  if (isContinuousMode.value) {
    await initializeContinuousChapters(store.currentIndex, false)
  } else {
    clearContinuousChapters()
    await nextTick()
    if (scrollContainerRef.value) {
      scrollContainerRef.value.scrollTo({ top: 0, left: 0, behavior: 'auto' })
    }
  }
  if (isHorizontalPageMode.value && scrollContainerRef.value) {
    resetHorizontalPagePosition()
  }
  await rebuildHorizontalPages()
  updateHorizontalEndState()
  scheduleRestoreReadingPosition()
})

watch(() => store.currentIndex, () => {
  if (!isHorizontalPageMode.value) return
  resetHorizontalPagePosition()
  scheduleRebuildHorizontalPages()
})

watch(
  [() => store.content, () => config.value.fontSize, () => config.value.fontWeight, () => config.value.lineHeight, () => config.value.paragraphSpacing, () => config.value.firstLineIndent, () => config.value.showChapterImages, showSearch, searchQuery],
  () => {
    if (isHorizontalPageMode.value) {
      horizontalPageIndex.value = 0
      scheduleRebuildHorizontalPages()
    }
  },
)

watch(() => store.currentIndex, async () => {
  loadSavedReadingPosition()
  resetAutoParagraphIndex()
  if (!store.isSpeaking) {
    clearReadingClass()
  }
  if (hideReadChaptersMode.value) {
    pruneReadChapters(store.currentIndex)
  }
  if (!isContinuousMode.value && config.value.enablePreload) {
    store.preloadAroundChapter(store.currentIndex)
  }
  if (isContinuousMode.value && !suppressContinuousSync.value) {
    await syncContinuousToStoreState()
  }
  scheduleRefreshOfflineCacheState()
  scheduleRestoreReadingPosition()
})

watch(
  [() => store.chapters.length, () => store.chaptersLoading, () => store.loading, isContinuousMode],
  async ([chapterCount, chaptersLoading, loadingNow, continuousMode]) => {
    if (!continuousMode || !chapterCount || chaptersLoading || loadingNow || continuousChapters.value.length) return
    await initializeContinuousChapters(store.currentIndex, false)
    scheduleRestoreReadingPosition()
  },
  { immediate: true },
)

watch(() => store.content, () => {
  resetAutoParagraphIndex()
  if (isContinuousMode.value) {
    const current = getContinuousChapter(store.currentIndex)
    if (current) {
      current.content = store.content
      current.html = renderChapterHtml(store.content, current.index)
    } else if (store.content) {
      void initializeContinuousChapters(store.currentIndex, false)
    }
  }
  handleContentChanged()
  handleContentUpdated()
  scheduleRefreshOfflineCacheState()
  scheduleRestoreReadingPosition()
})

// 配图比正文晚到：连续滚动模式的 HTML 是预渲染的，得为当前章补一次
watch(() => store.chapterImages, () => {
  if (!isContinuousMode.value) return
  const current = getContinuousChapter(store.currentIndex)
  if (current) current.html = renderChapterHtml(store.content, current.index)
})

watch(() => store.loading, (loading) => {
  if (!loading && pendingRestorePosition.value) {
    scheduleRestoreReadingPosition()
  }
})

watch(() => store.book?.bookUrl, () => {
  loadSavedReadingPosition()
  scheduleRefreshOfflineCacheState()
})

watch([showSearch, searchQuery, () => config.value.paragraphSpacing, () => config.value.firstLineIndent, () => config.value.showChapterImages, () => config.value.chineseMode, () => store.replaceRules], () => {
  if (isContinuousMode.value) {
    syncContinuousChapterHtml()
  }
  handlePresentationUpdated()
})

watch(() => config.value.selectAction, (value) => {
  if (value !== 'popup') {
    clearSelectionState()
  }
})

watch(() => store.isSpeaking, (speaking) => {
  if (speaking && !ttsPanelDismissed.value) {
    showTTSPanel.value = true
  }
  if (!speaking && !store.isAutoScrolling) {
    clearReadingClass()
  }
})

watch(
  [() => store.isNight, () => store.currentTheme.body, () => appStore.theme],
  ([isNight, body]) => {
    applySystemTheme(isNight ? 'dark' : appStore.theme, body)
  },
  { immediate: true },
)
</script>

<style scoped>
.reader-view {
  height: 100vh;
  height: 100dvh;
  height: var(--app-visual-height, var(--app-height, 100dvh));
  width: 100%;
  display: flex;
  position: relative;
  overflow: hidden;
  transition: background 0.3s, color 0.3s;
  padding-top: var(--safe-area-top);
  box-sizing: border-box;
}

.reader-view.disable-system-callout .chapter-text,
.reader-view.disable-system-callout .horizontal-page-content,
.reader-view.disable-system-callout .continuous-reading {
  -webkit-touch-callout: none;
}

.reader-scroll-container {
  flex: 1;
  height: 100%;
  overflow-y: auto;
  position: relative;
  scroll-behavior: smooth;
  overscroll-behavior: contain;
  -webkit-overflow-scrolling: touch;
  scrollbar-width: none;
  -ms-overflow-style: none;
}

.reader-scroll-container.horizontal-page-mode {
  overflow-x: hidden;
  overflow-y: hidden;
  touch-action: pan-y pinch-zoom;
  overscroll-behavior: none;
}

/* Hide scrollbar */
.reader-scroll-container::-webkit-scrollbar {
  width: 0;
  height: 0;
  display: none;
}
.reader-scroll-container::-webkit-scrollbar-thumb {
  background: rgba(0,0,0,0.1);
  border-radius: 4px;
}
.reader-view[style*="background: #1a1a2e"] .reader-scroll-container::-webkit-scrollbar-thumb {
  background: rgba(255,255,255,0.1);
}

.content-loading {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 100%;
}

.offline-banner {
  position: sticky;
  top: 0;
  z-index: 6;
  margin: 0 auto;
  width: min(100%, 880px);
  padding: 10px 16px;
  background: rgba(201, 127, 58, 0.12);
  color: var(--color-primary);
  border-bottom: 1px solid rgba(201, 127, 58, 0.18);
  font-size: var(--text-sm);
  line-height: 1.5;
  text-align: center;
  backdrop-filter: blur(6px);
}

.loading-spinner {
  width: 32px;
  height: 32px;
  border: 3px solid rgba(0,0,0,0.1);
  border-top-color: var(--color-primary);
  border-radius: 50%;
  animation: spin 1s linear infinite;
}
.reader-view[style*="background: #1a1a2e"] .loading-spinner {
  border-color: rgba(255,255,255,0.1);
  border-top-color: var(--color-primary);
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

.chapter-content {
  margin: 0 auto;
  padding: 80px 24px;
  min-height: 100%;
  transition: all 0.3s ease;
}

.chapter-content.horizontal-page-article {
  margin: 0;
  height: 100%;
  min-height: 100%;
  width: max-content;
  min-width: 100%;
  padding: 0;
}

.horizontal-page-layout {
  width: max-content;
  min-width: var(--reader-page-step);
  height: 100%;
}

.horizontal-content-page {
  width: max-content;
  min-width: var(--reader-page-step);
  height: 100%;
  min-height: 100%;
  padding: 0;
  box-sizing: border-box;
}

.horizontal-pages {
  display: flex;
  width: max-content;
  height: 100%;
  min-height: 100%;
  transform: translate3d(0, 0, 0);
  transition-property: transform;
  transition-timing-function: cubic-bezier(0.22, 0.61, 0.36, 1);
  will-change: transform;
}

.horizontal-page {
  width: var(--reader-page-step);
  min-width: var(--reader-page-step);
  height: 100%;
  min-height: 100%;
  padding: 24px var(--reader-side-padding);
  box-sizing: border-box;
}

.continuous-reading {
  margin: 0 auto;
  padding: 32px 0 80px;
}

.continuous-chapter {
  min-height: auto;
  padding-top: 48px;
  padding-bottom: 24px;
}

.load-error {
  padding: 48px 24px;
  text-align: center;
  color: var(--color-danger, #c0392b);
  font-size: var(--text-base);
  line-height: 1.6;
  word-break: break-all;
}

.load-error p {
  margin: 0 0 16px;
}

/* 与 next-btn 同一幽灵按钮语言：currentColor 跟随阅读器主题（错误态下继承 danger 红） */
.load-error .retry-btn {
  padding: 8px 24px;
  border-radius: 30px;
  background: transparent;
  border: 1px solid currentColor;
  color: inherit;
  font-size: var(--text-sm);
  opacity: 0.8;
  cursor: pointer;
  transition: all 0.2s;
}

.load-error .retry-btn:hover {
  opacity: 1;
}

.chapter-title {
  font-size: 1.6em;
  font-weight: 700;
  margin-bottom: 2em;
  text-align: center;
  line-height: 1.4;
}

.chapter-text {
  word-break: normal;
  overflow-wrap: anywhere;
  text-align: justify;
  user-select: text;
  -webkit-user-select: text;
  -webkit-touch-callout: default;
}

/* ─── 段评气泡：贴着段落末尾的小气泡，点开看这一段下面的评论 ─── */
:deep(.para-comment-bubble) {
  display: inline-flex;
  align-items: center;
  gap: 1px;
  /* 段落带 text-indent，而气泡内部的 span 是块容器会继承它，
     把首行缩进算进宽度里（29px 的字被撑到 65px）。这里必须清掉。 */
  text-indent: 0;
  text-align: left;
  /* 内容最长就是「999+」四个字符，别把气泡撑得比字还宽 */
  margin-left: 4px;
  padding: 0 4px 0 2px;
  border-radius: 999px;
  background: rgba(128, 128, 128, 0.14);
  color: var(--color-text-tertiary);
  font-size: 0.68em;
  line-height: 1.7;
  vertical-align: middle;
  white-space: nowrap;
  cursor: pointer;
  user-select: none;
  -webkit-user-select: none;
  transition: background 0.15s ease;
}

:deep(.para-comment-bubble:hover),
:deep(.para-comment-bubble:active) {
  background: rgba(212, 129, 42, 0.22);
  color: var(--color-primary-dark);
}

:deep(.para-comment-bubble svg) {
  width: 0.95em;
  height: 0.95em;
  flex: none;
  opacity: 0.75;
}

/* 作者评论/点赞过：气泡染上主题色，羽毛笔标记不透明 */
:deep(.para-comment-bubble.has-author) {
  background: rgba(201, 127, 58, 0.16);
  color: var(--color-primary, #c97f3a);
}

:deep(.para-comment-bubble.has-author:hover),
:deep(.para-comment-bubble.has-author:active) {
  background: rgba(212, 129, 42, 0.28);
}

:deep(.para-comment-bubble svg.para-author-mark) {
  width: 0.9em;
  height: 0.9em;
  margin-left: 1px;
  opacity: 1;
}

:deep(.para-comment-count) {
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.02em;
}

/* ─── 本章评论入口 ─── */
.chapter-comments-bar {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  margin-top: 28px;
  padding: 12px 16px;
  background: rgba(128, 128, 128, 0.1);
  border: none;
  border-radius: 12px;
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
  text-align: left;
  cursor: pointer;
  transition: background 0.15s ease;
}

.chapter-comments-bar:hover {
  background: rgba(128, 128, 128, 0.16);
}

.chapter-comments-label {
  font-weight: 600;
}

.chapter-comments-count {
  color: var(--color-text-tertiary);
}

.chapter-comments-arrow {
  width: 15px;
  height: 15px;
  margin-left: auto;
  opacity: 0.5;
}

.horizontal-page-content {
  height: 100%;
  overflow: hidden;
  overflow-wrap: break-word;
  text-align: left;
  word-break: normal;
}

:deep(.horizontal-page-content .horizontal-flow-title) {
  margin: 0 0 1em 0;
  font-size: 1.5em;
  line-height: 1.35;
  font-weight: 700;
  text-align: center;
  break-inside: avoid;
}

:deep(.horizontal-page-content p:first-child) {
  margin-top: 0 !important;
}

:deep(.horizontal-page-content p:last-child) {
  margin-bottom: 0 !important;
}

:deep(.chapter-text p.reading) {
  background: rgba(201, 127, 58, 0.12);
  border-radius: 10px;
  box-shadow: inset 0 0 0 1px rgba(201, 127, 58, 0.18);
}

:deep(.chapter-text p.reader-indent) {
  text-indent: 2em !important;
}

:deep(.chapter-text p) {
  text-indent: 0;
  user-select: text;
  -webkit-user-select: text;
}

/* ─── 章节配图：书源配图规则插进来的 figure，以及正文 HTML 自带的 <img> ─── */
:deep(.chapter-text img) {
  display: block;
  max-width: 100%;
  height: auto;
  margin: 0.6em auto;
  border-radius: 6px;
  cursor: zoom-in;
  /* 未加载完时按属性给出的宽高占位，避免正文跳动 */
  background: rgba(127, 127, 127, 0.08);
}

:deep(.chapter-figure) {
  margin: 1em 0;
}

:deep(.chapter-figure figcaption) {
  margin-top: 0.4em;
  text-align: center;
  font-size: 0.85em;
  opacity: 0.65;
  text-indent: 0;
}

.chapter-footer {
  margin-top: 60px;
  text-align: center;
  padding-bottom: 40px;
}

.horizontal-next-floating {
  position: absolute;
  left: 50%;
  bottom: calc(20px + var(--safe-area-bottom));
  transform: translateX(-50%);
  z-index: 12;
  pointer-events: none;
}

.horizontal-next-floating .next-btn {
  pointer-events: auto;
  background: rgba(255, 255, 255, 0.75);
  backdrop-filter: blur(6px);
}

.continuous-loading-inline {
  text-align: center;
  padding: 18px 24px;
  opacity: 0.6;
  font-size: var(--text-sm);
}

.next-btn {
  padding: 12px 36px;
  border-radius: 30px;
  background: transparent;
  border: 1px solid currentColor;
  color: inherit;
  font-size: var(--text-base);
  opacity: 0.6;
  cursor: pointer;
  transition: all 0.2s;
}

.next-btn:hover:not(:disabled) {
  opacity: 1;
  background: rgba(0,0,0,0.05);
}

.next-btn:disabled {
  opacity: 0.2;
  cursor: not-allowed;
}



/* Slide Drawer Overlay */
.reader-overlay {
  position: fixed;
  inset: 0;
  background: var(--overlay-mask-bg);
  -webkit-backdrop-filter: var(--overlay-mask-blur);
  backdrop-filter: var(--overlay-mask-blur);
  z-index: 40;
}

.reader-drawer {
  position: fixed;
  top: var(--safe-area-top);
  bottom: var(--safe-area-bottom);
  left: 0;
  width: var(--sidebar-width);
  z-index: 50;
  box-shadow: 4px 0 24px rgba(0,0,0,0.15);
  transition: background 0.3s;
}

.selection-menu {
  position: fixed;
  z-index: 60;
  min-width: 220px;
  max-width: min(320px, calc(100vw - 32px));
  border-radius: 14px;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.18);
  border: 1px solid rgba(0, 0, 0, 0.06);
  overflow: hidden;
}

.selection-menu-text {
  padding: 12px 14px 8px;
  font-size: var(--text-sm);
  line-height: 1.5;
  opacity: 0.72;
  word-break: break-all;
}

.selection-menu-actions {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px;
  padding: 0 12px 12px;
}

.selection-menu-actions button {
  border: none;
  border-radius: 10px;
  padding: 10px 12px;
  background: var(--color-primary);
  color: #fff;
  font-size: var(--text-sm);
  cursor: pointer;
}

.selection-menu-actions button:first-child {
  grid-column: 1 / -1;
}

/* 右键菜单形态：竖排列表，占满整宽，像原生上下文菜单 */
.selection-menu.as-context-menu {
  display: flex;
  flex-direction: column;
  min-width: 150px;
  max-width: 260px;
  padding: 3px 0;
  border-radius: 8px;
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.2);
}

.selection-menu.as-context-menu .selection-menu-text {
  margin: 0 0 2px;
  padding: 6px 12px 7px;
  border-bottom: 1px solid rgba(128, 128, 128, 0.2);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  opacity: 0.55;
}

.selection-menu.as-context-menu .selection-menu-actions {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: 0;
  padding: 2px 0 0;
}

.selection-menu.as-context-menu .selection-menu-actions button {
  width: 100%;
  padding: 8px 14px;
  border-radius: 0;
  background: transparent;
  color: inherit;
  text-align: left;
}

.selection-menu.as-context-menu .selection-menu-actions button:hover {
  background: rgba(128, 128, 128, 0.12);
}

.selection-menu.as-context-menu .selection-menu-actions button:first-child {
  grid-column: auto;
}

:deep(.search-highlight) {
  background: yellow;
  color: black;
  border-radius: 2px;
}

:deep(.search-highlight.current-match) {
  background: orange;
}

@media (max-width: 767px) {
  .reader-scroll-container.horizontal-page-mode {
    scroll-behavior: auto;
  }

  .chapter-content {
    padding: 24px 20px 8px;
    min-height: auto;
    height: auto;
  }

  .continuous-reading {
    padding: 16px 0 8px;
  }

  .continuous-chapter {
    padding-top: 20px;
    padding-bottom: 8px;
  }

  .chapter-title {
    margin-bottom: 0.9em;
  }

  .chapter-footer {
    margin-top: 12px;
    padding-bottom: 0;
  }

  .reader-drawer {
    top: var(--safe-area-top);
    bottom: var(--safe-area-bottom);
    width: min(340px, 85vw);
    padding-top: var(--safe-area-top);
    padding-bottom: var(--safe-area-bottom);
    box-sizing: border-box;
  }
}

/* Transitions */
.fade-enter-active, .fade-leave-active { transition: opacity 0.3s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }

.slide-left-enter-active, .slide-left-leave-active { transition: transform 0.35s cubic-bezier(0.2, 0.8, 0.2, 1); }
.slide-left-enter-from, .slide-left-leave-to { transform: translateX(-100%); }

</style>
