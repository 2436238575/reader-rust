<template>
  <div
    class="reader-view"
    :class="{
      'disable-system-callout': disableSystemCallout,
      'select-action-ignore': config.selectAction === 'ignore',
    }"
    :style="{
      background: theme.body,
      color: theme.fontColor,
      fontFamily: currentFontFamily,
      '--color-primary': '#c97f3a',
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
        <div
          v-if="store.activePanel"
          class="reader-drawer"
          :style="{ background: chromeTheme.popup }"
        >
          <ReaderCatalog
            v-if="store.activePanel === 'catalog' || store.activePanel === 'bookmark'"
            :initial-tab="store.activePanel === 'bookmark' ? 'bookmarks' : 'chapters'"
            @jump-chapter="jumpFromCatalog"
            @jump-bookmark="jumpToBookmark"
          />
          <ReadSettings
            v-else-if="store.activePanel === 'settings'"
            @view-diff="openDiffPanel"
          />
          <ReaderBookshelf v-else-if="store.activePanel === 'bookshelf'" />
          <ReaderSource v-else-if="store.activePanel === 'source'" />
          <ReplaceRuleManager v-else-if="store.activePanel === 'rule'" />
          <KeywordHighlightManager v-else-if="store.activePanel === 'keyword'" />
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
      @search="toggleSearch"
      @info="openInfo"
      @tts="handleTTS"
      @prev="prevChapter"
      @next="nextChapter"
      @progress="openCachePanel"
      @refresh="handleRefreshContent"
      @history="openVersionPanel"
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
      @search="openSearch"
      @info="openInfo"
      @tts="handleTTS"
      @progress="openCachePanel"
      @history="openVersionPanel"
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
      :chapter-edited="currentChapterEdited"
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
      :book-id="store.book?.bookId || ''"
      :chapter-url="store.currentChapter?.url || ''"
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
      @mousedown="interruptAutoScroll"
      @wheel="interruptAutoScroll"
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
          maxWidth: isHorizontalPageMode
            ? 'none'
            : `min(${config.pageWidth}px, 100% - ${isMobile ? 0 : 176}px)`,
          fontSize: config.fontSize + 'px',
          fontWeight: config.fontWeight,
          lineHeight: config.lineHeight,
          '--reader-page-width': config.pageWidth + 'px',
          '--reader-side-padding': '24px',
          '--reader-page-step': horizontalPageStepStyle,
          '--reader-page-window': horizontalPageWindowStyle,
          '--reader-page-content-height': horizontalPageContentHeightStyle,
        }"
      >
        <div
          v-if="isHorizontalPageMode"
          class="horizontal-page-layout"
          :class="{ 'dual-page-mode': isDualPageMode }"
        >
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
              <section
                v-for="(page, idx) in horizontalPages"
                :key="`h-page-${idx}`"
                class="horizontal-page"
              >
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
              <svg
                class="chapter-comments-arrow"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
              >
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
        <div
          v-if="!store.loading && isHorizontalPageMode && isHorizontalAtEnd"
          class="horizontal-next-floating"
        >
          <button
            v-if="store.reviewEnabled"
            class="chapter-comments-bar horizontal-comments-entry"
            @click="openChapterComments"
          >
            <span class="chapter-comments-label">本章评论</span>
            <span v-if="store.chapterCommentTotal > 0" class="chapter-comments-count">
              · {{ store.chapterCommentTotal }}
            </span>
          </button>
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
          :style="chapter.prunedHeight ? { minHeight: chapter.prunedHeight + 'px' } : undefined"
        >
          <!-- 裁剪占位章（上方/下方对称）：保高度不渲染内容，回到视口附近时回载 -->
          <div v-if="chapter.prunedHeight" class="chapter-placeholder">{{ chapter.title }}</div>
          <template v-else>
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

            <div
              v-if="chapter.index === continuousChapters[continuousChapters.length - 1]?.index"
              class="chapter-footer"
            >
              <button class="next-btn" :disabled="!store.hasNext" @click="nextChapter">
                {{ store.hasNext ? '继续加载下一章' : '已经到底了' }}
              </button>
            </div>
          </template>
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
      :scope="searchScope"
      :loading="bookSearchLoading"
      @close="closeSearch"
      @search="runSearch"
      @next="nextSearchResult"
      @prev="prevSearchResult"
      @update:query="searchQuery = $event"
      @update:scope="setSearchScope"
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
          <button @click="addSelectionBookmark">加入划线</button>
          <button @click="addSelectionKeywordHighlight">加入高亮</button>
          <button @click="addSelectionReplaceRule('book')">按本书替换</button>
          <button @click="addSelectionReplaceRule('source')">按书源替换</button>
        </div>
      </div>
    </Transition>

    <!-- 已回显划线的操作菜单：与选区菜单同形态（胶囊/右键竖排跟随选字模式），
         「加入划线」换成「删除划线」 -->
    <Transition name="fade">
      <div
        v-if="underlineMenu.visible"
        class="selection-menu"
        :class="{ 'as-context-menu': underlineMenu.variant === 'context' }"
        @click.stop
        :style="{
          top: underlineMenu.top + 'px',
          left: underlineMenu.left + 'px',
          background: chromeTheme.popup,
          color: chromeTheme.fontColor,
        }"
      >
        <div class="selection-menu-text">
          {{
            underlineMenu.text.length > 48
              ? underlineMenu.text.slice(0, 48) + '...'
              : underlineMenu.text
          }}
        </div>
        <div class="selection-menu-actions">
          <button class="danger-action" @click="deleteUnderlineFromMenu">删除划线</button>
          <button @click="underlineMenuAddHighlight">加入高亮</button>
          <button @click="underlineMenuAddReplaceRule('book')">按本书替换</button>
          <button @click="underlineMenuAddReplaceRule('source')">按书源替换</button>
        </div>
      </div>
    </Transition>

    <!-- 点击 diff 标记的就地预览：本段「原 → 改」，或这段前面的删除块 -->
    <Transition name="fade">
      <div
        v-if="diffPreview.visible && diffPreview.model"
        class="selection-menu diff-preview"
        @click.stop
        :style="{
          top: diffPreview.top + 'px',
          left: diffPreview.left + 'px',
          background: chromeTheme.popup,
          color: chromeTheme.fontColor,
        }"
      >
        <div class="diff-preview-head">
          <span>
            {{
              diffPreview.model.kind === 'add'
                ? '新增段落'
                : diffPreview.model.kind === 'delete'
                  ? '删除的段落'
                  : '本段改动'
            }}
          </span>
          <button class="diff-preview-close" aria-label="关闭" @click="closeDiffPreview">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </div>
        <div class="diff-preview-body">
          <template v-if="diffPreview.model.kind === 'modify'">
            <div class="diff-preview-row del">
              <span class="diff-preview-gutter">-</span>
              <span>{{ diffPreview.model.baseText }}</span>
            </div>
            <div class="diff-preview-row add">
              <span class="diff-preview-gutter">+</span>
              <span>
                <template
                  v-for="(part, partIndex) in diffPreviewParts(diffPreview.model)"
                  :key="partIndex"
                >
                  <span v-if="part.added" class="diff-preview-ins">{{ part.text }}</span>
                  <template v-else>{{ part.text }}</template>
                </template>
              </span>
            </div>
          </template>
          <div v-else-if="diffPreview.model.kind === 'add'" class="diff-preview-row add">
            <span class="diff-preview-gutter">+</span>
            <span>{{ diffPreview.model.editedText }}</span>
          </div>
          <template v-if="diffPreview.model.deletedTotal">
            <div class="diff-preview-label">删除 {{ diffPreview.model.deletedTotal }} 段</div>
            <div
              v-for="(text, delIndex) in diffPreview.model.deleted.slice(0, 3)"
              :key="delIndex"
              class="diff-preview-row del"
            >
              <span class="diff-preview-gutter">-</span>
              <span>{{ text }}</span>
            </div>
            <div v-if="diffPreview.model.deletedTotal > 3" class="diff-preview-more">
              还有 {{ diffPreview.model.deletedTotal - 3 }} 段，打开完整面板查看
            </div>
          </template>
        </div>
        <div class="diff-preview-actions">
          <button @click="openDiffPanelFromPreview">查看全部</button>
        </div>
      </div>
    </Transition>

    <BookDetailModal v-model="showBookInfo" :book="bookInfoBook" @switched="handleBookSwitched" />

    <ChapterEditorModal
      v-model="showChapterEditor"
      :seed="chapterEditorSeed"
      @changed="handleChapterEditChanged"
    />

    <ChapterVersionPanel v-model="showVersionPanel" @edit="openChapterEditorFromVersion" />

    <ChapterDiffPanel
      v-model="showDiffPanel"
      :ops="currentChapterDiffOps"
      :error="currentChapterDiffError"
      :viewing-upstream="currentChapterViewingUpstream"
      @changed="handleChapterEditChanged"
    />

    <ImageLightbox :src="previewImage" @close="previewImage = ''" />
  </div>
</template>

<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted, nextTick, defineAsyncComponent } from 'vue'
import { onBeforeRouteLeave, useRouter } from 'vue-router'
import { useReaderStore, fontPresets } from '../stores/reader'
import { useAppStore } from '../stores/app'
import { useSourceStore } from '../stores/source'
import { applySystemTheme } from '../utils/systemUi'
import { isIosWebkit } from '../utils/platform'
import { APP_VIEWPORT_CHANGE_EVENT, syncViewportSize } from '../utils/viewport'
import { isReaderInteractiveClickTarget, resolveReaderTapZone } from '../utils/readerClick'
import { chapterPosToProgress } from '../utils/bookmarks'
import { darkenKeywordColor } from '../utils/keywordHighlights'
import { buildDiffPreview, inlineDiff, type DiffPreviewModel } from '../utils/chapterDiff'
import { DUAL_PAGE_MIN_WIDTH, useHorizontalPaging } from '../composables/useHorizontalPaging'
import {
  appendLocalEpubAssetAuth,
  applyDiffMarks,
  applyUnderlineMarks,
  buildBaseChapterHtml,
  buildParaBubbleMap,
  highlightKeywords,
  highlightSearchText,
  insertChapterImages,
  renderParaCommentBubble,
  splitParagraphLines,
  type BaseRenderOptions,
  type UnderlineMarkSpec,
} from '../utils/chapterRender'
import type { ChapterImage } from '../types'
import {
  createReaderProgressAutoSaveScheduler,
  createReaderProgressExitSaver,
} from '../utils/readerProgressAutoSave'
import type { Book, SearchBook } from '../types'

import ReaderSidebar from '../components/reader/ReaderSidebar.vue'
import ReaderToolbar from '../components/reader/ReaderToolbar.vue'
import ReaderMobileControls from '../components/reader/ReaderMobileControls.vue'
import { useReaderSearch } from '../composables/useReaderSearch'
import { useReaderSelection } from '../composables/useReaderSelection'
import { useContinuousReading } from '../composables/useContinuousReading'
import { useReaderAutoPlayback } from '../composables/useReaderAutoPlayback'
import { useOfflineBanner } from '../composables/useOfflineBanner'
import { useCommentPanelGlue } from '../composables/useCommentPanelGlue'
import { useOverlayHistory } from '../composables/useOverlayHistory'
import { useReaderKeyboard } from '../composables/useReaderKeyboard'
import { useReaderTtsControls } from '../composables/useReaderTtsControls'
import { useBookInfoModal } from '../composables/useBookInfoModal'
import {
  useReadingPosition,
  CONTINUOUS_POSITION_ANCHOR_RATIO,
} from '../composables/useReadingPosition'

const ReaderCatalog = defineAsyncComponent(() => import('../components/reader/ReaderCatalog.vue'))
const ReadSettings = defineAsyncComponent(() => import('../components/reader/ReadSettings.vue'))
const ReaderBookshelf = defineAsyncComponent(
  () => import('../components/reader/ReaderBookshelf.vue')
)
const ReaderSource = defineAsyncComponent(() => import('../components/reader/ReaderSource.vue'))
const ReplaceRuleManager = defineAsyncComponent(
  () => import('../components/reader/ReplaceRuleManager.vue')
)
const KeywordHighlightManager = defineAsyncComponent(
  () => import('../components/reader/KeywordHighlightManager.vue')
)
const CacheManager = defineAsyncComponent(() => import('../components/reader/CacheManager.vue'))
const BookDetailModal = defineAsyncComponent(() => import('../components/BookDetailModal.vue'))
const ReaderTtsPanel = defineAsyncComponent(() => import('../components/reader/ReaderTtsPanel.vue'))
const ReaderSearchPanel = defineAsyncComponent(
  () => import('../components/reader/ReaderSearchPanel.vue')
)
const CommentPanel = defineAsyncComponent(() => import('../components/reader/CommentPanel.vue'))
const ImageLightbox = defineAsyncComponent(() => import('../components/reader/ImageLightbox.vue'))
const ChapterEditorModal = defineAsyncComponent(
  () => import('../components/reader/ChapterEditorModal.vue')
)
const ChapterDiffPanel = defineAsyncComponent(
  () => import('../components/reader/ChapterDiffPanel.vue')
)
const ChapterVersionPanel = defineAsyncComponent(
  () => import('../components/reader/ChapterVersionPanel.vue')
)

const router = useRouter()
const store = useReaderStore()
const appStore = useAppStore()
const sourceStore = useSourceStore()
const SERVER_PROGRESS_AUTOSAVE_MS = 10000

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
let suppressNextTapUntil = 0
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
const isContinuousMode = computed(
  () => config.value.readMethod === '上下滚动' || config.value.readMethod === '上下滚动2'
)
const hideReadChaptersMode = computed(() => config.value.readMethod === '上下滚动2')
const isHorizontalPageMode = computed(
  () => config.value.readMethod === '左右翻页' || config.value.readMethod === '双页翻页'
)
// 双页只在宽度足够时生效：窗口收窄自动退回单页横翻
const windowWidth = ref(typeof window !== 'undefined' ? window.innerWidth : 1024)
const isDualPageMode = computed(
  () => config.value.readMethod === '双页翻页' && windowWidth.value >= DUAL_PAGE_MIN_WIDTH
)
const iosWebkit = isIosWebkit()
const disableSystemCallout = computed(() => {
  return iosWebkit && isMobile.value && config.value.selectAction === 'popup'
})
const touchState = ref({
  startX: 0,
  startY: 0,
  startAt: 0,
  moving: false,
  horizontalLocked: false,
})
const { offlineBannerText, scheduleRefreshOfflineCacheState, disposeOfflineBanner } =
  useOfflineBanner(store, appStore)
// 评论面板（showCommentPanel 等）、书籍详情（showBookInfo 等）、
// TTS 面板（showTTSPanel 等）的状态由各自 composable 提供，见下方实例化
const {
  showCommentPanel,
  commentMode,
  commentParaIndex,
  commentParaText,
  commentParaAuthorMarked,
  openChapterComments,
  openParaComments,
  closeCommentPanel,
} = useCommentPanelGlue(store)
const { showBookInfo, bookInfoBook, openInfo, handleBookSwitched } = useBookInfoModal(
  store,
  appStore,
  router
)
const {
  showSearch,
  searchQuery,
  searchResults,
  searchIndex,
  searchCount,
  bookSearchStatus,
  searchScope,
  bookSearchLoading,
  toggleSearch,
  openSearch,
  closeSearch,
  runSearch,
  setSearchScope,
  nextSearchResult,
  prevSearchResult,
  jumpToSearchResult,
  handleContentUpdated,
  handlePresentationUpdated,
} = useReaderSearch(store, { locateMatch: locateSearchMatch })

// 搜索命中定位：横翻模式容器 overflow hidden + transform 翻页，
// scrollIntoView 无效，换算成页码跳过去（函数声明提升，useReaderSearch 可先引用）
function locateSearchMatch(el: HTMLElement) {
  const page = el.closest('.horizontal-page')
  if (isHorizontalPageMode.value && page?.parentElement) {
    const idx = Array.from(page.parentElement.children).indexOf(page as Element)
    if (idx >= 0) {
      suppressHorizontalTransitionOnce()
      horizontalPageIndex.value = idx
      syncHorizontalPageState()
    }
    return
  }
  el.scrollIntoView({ block: 'center', behavior: 'smooth' })
}
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
  addSelectionKeywordHighlight,
  underlineMenu,
  openUnderlineMenu,
  closeUnderlineMenu,
  deleteUnderlineFromMenu,
  underlineMenuAddHighlight,
  underlineMenuAddReplaceRule,
  clearSelectionState,
  disposeSelection,
} = useReaderSelection(
  store,
  appStore,
  computed(() => ({ selectAction: config.value.selectAction })),
  scrollContainerRef
)

// resize 与 viewport 事件双通道都会调 checkMedia：合并到同一帧，
// 否则拖拽窗口/键盘弹起会连续多次全量分页
let checkMediaQueued = false
function checkMedia() {
  isMobile.value = window.innerWidth <= 767
  windowWidth.value = window.innerWidth
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
  const preset = fontPresets.find((p) => p.value === config.value.fontFamily)
  return preset ? preset.family : ''
})

const baseRenderOpts = computed<BaseRenderOptions>(() => ({
  showChapterImages: config.value.showChapterImages,
  firstLineIndent: config.value.firstLineIndent,
  paragraphSpacing: config.value.paragraphSpacing,
}))

// 基础渲染与装饰层实现已抽到 utils/chapterRender.ts（纯函数，无响应式状态）
const baseChapterHtml = computed(() =>
  buildBaseChapterHtml(store.displayContent || '', baseRenderOpts.value)
)

// 装饰层：划线回显 + 章节配图 + 段评气泡 + 本地 EPUB 资源改写 + 常驻关键词高亮，
// 作用在基础 HTML 之上。划线包的是 span（不在 walker 排除名单），最先注入——
// 后续关键词的 mark 还能进 span 内部叠加（下划线 + 底色共存）。
// 搜索高亮不走这里：它是渲染后的 DOM 级标记（见 applySearchHighlightToDom），
// 否则搜索框每敲一个字都会沿 formattedContent 依赖链重建整章 DOM
function decorateChapterHtml(baseHtml: string, withBubbles: boolean, chapterIndex?: number) {
  const wrapper = document.createElement('div')
  wrapper.innerHTML = baseHtml
  const targetChapterIndex = chapterIndex ?? store.currentIndex
  // diff 标记在装饰层最前：它改段级 class 与段内 span，后续 walker 都能穿过。
  // 上游原版不叠加标记（ops 的 editedIndex 对应的是编辑版段落）
  if (config.value.showDiffMarks && chapterDiffState(targetChapterIndex)?.edited) {
    const ops = chapterDiffOps(targetChapterIndex)
    if (ops) applyDiffMarks(wrapper, ops)
  }
  applyUnderlineMarks(wrapper, underlineTextsForChapter(targetChapterIndex))
  if (withBubbles) {
    // 配图先插：段评气泡按 `<p>` 顺序定位，figure 不是 <p> 不影响它，
    // 但气泡会往段落里塞计数，得赶在它之前比对段落原文
    insertChapterImages(wrapper, chapterImageMap.value)
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
  // 常驻关键词高亮注入 mark.kw-highlight；搜索高亮已移到渲染后 DOM 级处理，
  // 不再参与 HTML 字符串构建（applySearchHighlightToDom，见 watch 群）
  highlightKeywords(wrapper, activeKeywordMarks.value)
  return wrapper.innerHTML
}

/** 某章的全部划线（回显用）：摘录文本 + 书签定位键 chapterPos。 */
function underlineTextsForChapter(chapterIndex: number): UnderlineMarkSpec[] {
  return store.bookmarks
    .filter((bm) => bm.kind === 'underline' && bm.chapterIndex === chapterIndex)
    .map((bm) => ({ text: bm.bookText || '', chapterPos: bm.chapterPos ?? 0 }))
    .filter((mark) => mark.text.trim().length >= 2)
}

/** 生效中的常驻关键词（启用 + scope 命中当前书），供装饰层上底色。
 *  夜间模式底色转成同色相的暗色变体：色盘值是浅色主题下调的半透明浅色，
 *  直接盖在深底上和浅色正文对比度太低。 */
const activeKeywordMarks = computed(() =>
  store.keywordRules
    .filter(
      (rule) =>
        rule.enabled &&
        rule.keyword.trim() &&
        (rule.scope === 'global' || (!!store.book?.bookId && rule.bookId === store.book.bookId))
    )
    .map((rule) => ({
      keyword: rule.keyword,
      color: store.isNight ? darkenKeywordColor(rule.color) : rule.color,
    }))
)

/* ─── 编辑版 diff（正文标记、就地预览与 diff 面板共用） ───
 * 差异随正文接口一起返回，store.chapterDiffs 是响应式 Map：
 * 横翻的 formattedContent 与连续模式渲染都直接读它。 */
function chapterDiffOps(chapterIndex: number) {
  return store.chapterDiffs.get(chapterIndex)?.ops ?? null
}

function chapterDiffState(chapterIndex: number) {
  return store.chapterDiffs.get(chapterIndex) ?? null
}

async function handleRefreshContent() {
  await store.refreshContent()
}

/* ─── 章节正文编辑 ─── */
const showChapterEditor = ref(false)
const showDiffPanel = ref(false)
const showVersionPanel = ref(false)
/** 当前章的 diff 操作序列（store 的响应式 Map，面板跟着重算） */
const currentChapterDiffOps = computed(() => chapterDiffOps(store.currentIndex))
/** diff 算不出来的原因：面板要显示「无法对比」而不是「没有差异」 */
const currentChapterDiffError = computed(
  () => store.chapterDiffs.get(store.currentIndex)?.error || ''
)
/** 当前是否在读上游原版（diff 面板里提示一句） */
const currentChapterViewingUpstream = computed(
  () => store.chapterDiffs.get(store.currentIndex)?.variant === 'upstream'
)
/** 当前章是否有编辑版（决定工具栏编辑按钮的去向） */
const currentChapterEdited = computed(() => {
  const chapter = store.currentChapter
  return !!chapter && store.editedChapterKeys.has(chapter.url)
})

/** 编辑器底稿：从正文历史「编辑」某版本时预填该版本正文；空表示编辑当前显示内容 */
const chapterEditorSeed = ref<{ content: string; versionId?: number } | null>(null)

function openChapterEditor(seed: { content: string; versionId?: number } | null = null) {
  if (!store.currentChapter) return
  chapterEditorSeed.value = seed
  showDiffPanel.value = false
  showChapterEditor.value = true
}

/** 正文历史面板的「编辑」：关掉面板开编辑器；带 seed 时用预览版本做底稿 */
function openChapterEditorFromVersion(seed: { content: string; versionId?: number } | null) {
  showVersionPanel.value = false
  openChapterEditor(seed)
}

/** 「查看本章改动」：diff 已经随正文回来，直接开面板 */
function openDiffPanel() {
  if (!store.currentChapter) return
  showDiffPanel.value = true
}

/** 「正文历史」：版本时间线、预览、切换当前版本、导入历史版本。 */
function openVersionPanel() {
  if (!store.currentChapter) return
  showVersionPanel.value = true
}

/** 编辑保存/恢复上游后：重载本章（响应里带最新正文与 diff） */
async function handleChapterEditChanged() {
  await store.reloadChapterAfterEdit(store.currentIndex)
}

function formatChapterHtml(rawText: string, withParaComments = false, chapterIndex?: number) {
  const base = withParaComments
    ? baseChapterHtml.value
    : buildBaseChapterHtml(rawText, baseRenderOpts.value)
  // 空正文不能提前短路：纯图章节（出版书「文前」）没有文字，章节配图在
  // 装饰层插入。非气泡路径维持短路——连续滚动模式自己按当前章插配图。
  if (!base && !withParaComments) return ''
  return decorateChapterHtml(base, withParaComments, chapterIndex)
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

const paraBubbleMap = computed(() =>
  buildParaBubbleMap(
    store.paraReviewEnabled ? store.paraReviewIndex : [],
    splitParagraphLines(store.content),
    rawLineByPosition.value,
    // 只有显示编辑版时才需要按原文重定位；上游原版的段号本来就对得上
    currentChapterEdited.value && chapterDiffState(store.currentIndex)?.edited === true
  )
)

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

/** 从事件目标解析划线：span 上的 chapterPos + 所在章（连续模式读祖先 section 的章号）。 */
function resolveUnderlineTarget(target: HTMLElement | null) {
  const span = target?.closest?.('.reader-underline') as HTMLElement | null
  if (!span) return null
  const chapterPos = Number(span.dataset.chapterPos)
  if (!Number.isFinite(chapterPos)) return null
  const sectionIndex = span
    .closest('section.continuous-chapter')
    ?.getAttribute('data-chapter-index')
  const chapterIndex = sectionIndex != null ? Number(sectionIndex) : store.currentIndex
  if (!Number.isFinite(chapterIndex)) return null
  return { text: span.textContent || '', chapterIndex, chapterPos }
}

/* ─── 点 diff 标记就地预览 ─── */
const diffPreview = ref<{
  visible: boolean
  top: number
  left: number
  model: DiffPreviewModel | null
}>({ visible: false, top: 0, left: 0, model: null })

function closeDiffPreview() {
  diffPreview.value.visible = false
}

/** 从点击目标解析「哪一章、哪一段」的 diff 标记（属性由 applyDiffMarks 写入）。 */
function resolveDiffPreviewTarget(target: HTMLElement | null) {
  const paragraph = target?.closest?.('p') as HTMLElement | null
  if (!paragraph) return null
  const opIndex = Number(paragraph.dataset.diffOp)
  const delStart = Number(paragraph.dataset.diffDelStart)
  const hasOp = Number.isFinite(opIndex)
  const hasDel = Number.isFinite(delStart)
  if (!hasOp && !hasDel) return null
  const sectionIndex = paragraph
    .closest('section.continuous-chapter')
    ?.getAttribute('data-chapter-index')
  const chapterIndex = sectionIndex != null ? Number(sectionIndex) : store.currentIndex
  if (!Number.isFinite(chapterIndex)) return null
  return {
    chapterIndex,
    opIndex: hasOp ? opIndex : null,
    delStart: hasDel ? delStart : null,
    paragraph,
  }
}

/** 打开就地预览；返回 false 表示这一点没有可展示的差异。 */
function openDiffPreview(target: HTMLElement | null) {
  const hit = resolveDiffPreviewTarget(target)
  if (!hit) return false
  const model = buildDiffPreview(chapterDiffOps(hit.chapterIndex), hit.opIndex, hit.delStart)
  if (!model) return false
  const rect = hit.paragraph.getBoundingClientRect()
  // 卡片高度按视口估算：下方放不下就翻到段落上方
  const cardHeight = Math.min(320, Math.max(160, window.innerHeight * 0.4))
  let top = rect.bottom + 10
  if (top + cardHeight > window.innerHeight - 16) {
    top = Math.max(16, rect.top - cardHeight - 10)
  }
  const left = Math.min(Math.max(16, rect.left), Math.max(16, window.innerWidth - 440))
  diffPreview.value = { visible: true, top, left, model }
  return true
}

/** 段内对照：编辑版里新增的部分加底色（与 diff 面板同口径）。 */
function diffPreviewParts(model: DiffPreviewModel) {
  const diff = inlineDiff(model.baseText || '', model.editedText || '')
  const parts: { text: string; added: boolean }[] = []
  if (diff.prefix) parts.push({ text: diff.prefix, added: false })
  if (diff.added) parts.push({ text: diff.added, added: true })
  if (diff.suffix) parts.push({ text: diff.suffix, added: false })
  return parts
}

function openDiffPanelFromPreview() {
  closeDiffPreview()
  openDiffPanel()
}

function handleChapterTextClick(event: MouseEvent) {
  if (Date.now() < suppressNextTapUntil) return
  const target = event.target as HTMLElement | null
  // 已回显的划线：点开操作菜单。只在「操作弹窗」模式下响应点击——
  // 「右键菜单」模式下划线归右键开，「忽略」模式下划线只读
  if (config.value.selectAction === 'popup') {
    const underline = resolveUnderlineTarget(target)
    if (underline) {
      event.stopPropagation()
      openUnderlineMenu(
        event.clientX,
        event.clientY,
        underline.text,
        underline.chapterIndex,
        underline.chapterPos
      )
      return
    }
  }
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
    return
  }
  // 编辑版 diff 标记：点段落就地看「原 → 改」（有文字选区时不抢）
  if (!window.getSelection?.()?.toString().trim() && openDiffPreview(target)) {
    event.stopPropagation()
  }
}

/* ─── 图片放大预览 ─── */
const previewImage = ref('')

/**
 * 渲染某一章的正文 HTML。
 *
 * `chapterIndex` 用于章节配图：配图是「当前章」的状态（后端按章返回），
 * 连续滚动模式下只有当前章该插配图，前后预加载的章节不插。
 */
function renderChapterHtml(rawText: string, chapterIndex?: number) {
  const html = formatChapterHtml(store.processContentForDisplay(rawText || ''), false, chapterIndex)
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
  horizontalPageWindowStyle,
  horizontalPageStride,
  horizontalPages,
  isHorizontalAtEnd,
  horizontalPageContentHeightStyle,
  horizontalInstantJump,
  suppressHorizontalTransitionOnce,
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
    pageWidth: config.value.pageWidth,
  })),
  currentFontFamily,
  formattedContent,
  isHorizontalPageMode,
  scrollContainerRef,
  isDualPageMode
)

const horizontalPageTransform = computed(() => {
  const offset = horizontalPageIndex.value * Math.max(1, horizontalPageStep.value)
  return `translate3d(${-offset}px, 0, 0)`
})
const horizontalPageTransitionDuration = computed(() => {
  if (horizontalInstantJump.value) return '0ms'
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
  rehydrateContinuousChapter,
  clearContinuousChapters,
  disposeContinuousReading,
} = useContinuousReading(
  store,
  renderChapterHtml,
  isContinuousMode,
  hideReadChaptersMode,
  scrollContainerRef
)

// 阅读位置的保存与恢复（本地+服务端选新、三模式分支、重试与抑制窗）收进 composable
const {
  pendingRestorePosition,
  queueRestorePosition,
  loadSavedReadingPosition,
  saveReadingPosition,
  scheduleSaveReadingPosition,
  scheduleRestoreReadingPosition,
  suppressContinuousInteractions,
  isContinuousScrollSyncSuppressed,
  isContinuousAutoLoadSuppressed,
  disposeReadingPosition,
} = useReadingPosition({
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
})

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
    const stride = horizontalPageStride.value
    const maxPage = Math.max(0, horizontalPages.value.length - 1)
    if (horizontalPageIndex.value + stride > maxPage) {
      nextChapter()
      return
    }
    horizontalPageIndex.value = Math.min(maxPage, horizontalPageIndex.value + stride)
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
      prevChapter(true)
      return
    }
    horizontalPageIndex.value = Math.max(0, horizontalPageIndex.value - horizontalPageStride.value)
    container.scrollTo({ left: 0, behavior: 'auto' })
    syncHorizontalPageState()
    return
  }
  const step = container.clientHeight * 0.88
  if (container.scrollTop <= 10) {
    prevChapter(true)
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

async function prevChapter(toChapterEnd = false) {
  const targetIndex = store.currentIndex - 1
  if (targetIndex < 0) return

  if (!isContinuousMode.value) {
    await store.prevChapter()
    if (toChapterEnd) {
      // 「章头再翻上一页」是在往回读：落到上一章章末，而不是章头。
      // 走恢复通道（与书签跳转同路）——横翻落最后一页、竖滚滚到底
      queueRestorePosition({
        chapterIndex: targetIndex,
        progress: 1,
        updatedAt: Date.now(),
      })
      scheduleRestoreReadingPosition()
      return
    }
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

// 书签跳转：loadChapter 会把进度归 0 并按服务端/本地记录恢复，
// 书签的 chapterPos（万分比章内进度）优先级更高——等正文加载完后用恢复通道落到收藏位置
async function jumpToBookmark(bm: { chapterIndex?: number; chapterPos?: number }) {
  const targetIndex = bm.chapterIndex
  if (targetIndex === undefined || targetIndex < 0 || targetIndex >= store.chapters.length) return
  const progress = chapterPosToProgress(bm.chapterPos)

  if (!isContinuousMode.value) {
    await store.loadChapter(targetIndex)
    queueRestorePosition({
      chapterIndex: targetIndex,
      progress,
      updatedAt: Date.now(),
    })
    scheduleRestoreReadingPosition()
    store.closePanel()
    return
  }

  await rebuildContinuousAtChapter(targetIndex)
  // 连续模式此前直接丢弃 chapterPos（只到章首），划线同样落到收藏位置
  if (progress > 0) {
    queueRestorePosition({
      chapterIndex: targetIndex,
      progress,
      updatedAt: Date.now(),
    })
    scheduleRestoreReadingPosition()
  }
  store.closePanel()
}

async function rebuildContinuousAtChapter(targetIndex: number) {
  suppressContinuousInteractions(500)
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
    scrollContainerRef.value.scrollTo({
      top: scrollContainerRef.value.scrollHeight,
      behavior: 'smooth',
    })
  }
}

// 点按/滚轮/按键打断自动滚动的时间戳：打断后紧接着的那次 tap 只表示
// 「停」，不再附带翻页或弹菜单（handleGlobalClick 据此吞掉）
let autoScrollInterruptAt = 0

function interruptAutoScroll() {
  if (!store.isAutoScrolling) return
  autoScrollInterruptAt = Date.now()
  stopAutoScroll()
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
  prevChapter
)

// TTS 面板粘合（开关/暂停切换/参数调整）收进 composable
const {
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
} = useReaderTtsControls(store, {
  startSpeech,
  cancelSpeechTransition,
  restartSpeechFromCurrentParagraph,
})

// 覆盖层历史栈：面板打开压入同 URL 历史标记，返回键只关最上层面板
// （panels 顺序即 ESC/返回键的关闭优先级）
const { handleOverlayPopState } = useOverlayHistory([
  { isOpen: computed(() => !!store.activePanel), close: () => store.closePanel() },
  { isOpen: showSearch, close: closeSearch },
  { isOpen: showCommentPanel, close: () => (showCommentPanel.value = false) },
  { isOpen: computed(() => !!previewImage.value), close: () => (previewImage.value = '') },
  { isOpen: showTTSPanel, close: closeTTSPanel },
  { isOpen: showBookInfo, close: () => (showBookInfo.value = false) },
])

const { handleKeydown } = useReaderKeyboard(
  store,
  appStore,
  {
    // ESC 关闭链顺序（优先级从高到低）：面板 → 选字菜单 → 搜索 → 评论 →
    // 图片预览 → TTS 面板 → 书籍详情 → 点按控制条 → 退出阅读页
    escChain: [
      { isOpen: () => !!store.activePanel, close: () => store.closePanel() },
      { isOpen: () => showDiffPanel.value, close: () => (showDiffPanel.value = false) },
      { isOpen: () => showChapterEditor.value, close: () => (showChapterEditor.value = false) },
      { isOpen: () => diffPreview.value.visible, close: closeDiffPreview },
      { isOpen: () => selectionMenu.value.visible, close: hideSelectionMenu },
      { isOpen: () => showSearch.value, close: closeSearch },
      { isOpen: () => showCommentPanel.value, close: () => (showCommentPanel.value = false) },
      { isOpen: () => !!previewImage.value, close: () => (previewImage.value = '') },
      { isOpen: () => showTTSPanel.value, close: closeTTSPanel },
      { isOpen: () => showBookInfo.value, close: () => (showBookInfo.value = false) },
    ],
    isBlockingOverlayOpen: () =>
      !!(
        store.activePanel ||
        showDiffPanel.value ||
        showChapterEditor.value ||
        showSearch.value ||
        showCommentPanel.value ||
        showBookInfo.value ||
        previewImage.value
      ),
    isControlsVisible: () => showControls.value,
    hideControls: () => (showControls.value = false),
  },
  {
    goBack,
    pageForward,
    pageBackward,
    nextChapter,
    prevChapter,
    scrollToTop,
    scrollToBottom,
    scrollPageVertical: (direction) => {
      const container = scrollContainerRef.value
      if (!container) return
      container.scrollBy({ top: direction * container.clientHeight * 0.8, behavior: 'smooth' })
    },
    isHorizontalPageMode: () => isHorizontalPageMode.value,
    hasScrollContainer: () => !!scrollContainerRef.value,
    interruptAutoScroll,
  }
)

// Click behavior
function handleBackgroundClick(e: Event) {
  // If clicked directly on the reader-view wrapper, toggle controls
  if ((e.target as HTMLElement).classList.contains('reader-view')) {
    showControls.value = false
  }
}

function handleContextMenu(event: MouseEvent) {
  // 「右键菜单」模式：右键点在已回显的划线上，打开划线操作菜单（竖排形态）
  if (config.value.selectAction === 'contextmenu') {
    const underline = resolveUnderlineTarget(event.target as HTMLElement | null)
    if (underline) {
      event.preventDefault()
      openUnderlineMenu(
        event.clientX,
        event.clientY,
        underline.text,
        underline.chapterIndex,
        underline.chapterPos
      )
      return
    }
    // 选区上右键唤出操作菜单
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
  if (underlineMenu.value.visible) {
    closeUnderlineMenu()
    return
  }
  if (selectionMenu.value.visible) {
    hideSelectionMenu()
    return
  }
  if (diffPreview.value.visible) {
    closeDiffPreview()
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
  // 这次点按刚用于停掉自动滚动（touchstart/mousedown 已先行处理），
  // 不再附带翻页或弹菜单动作
  if (Date.now() - autoScrollInterruptAt < 400) return

  // 九宫格热区（横翻永远左右分区，其余模式上下偏置），四边留白不触发
  const zone = resolveReaderTapZone(
    e.clientX,
    e.clientY,
    isHorizontalPageMode.value,
    window.innerWidth,
    window.innerHeight
  )
  if (zone) clickZoneAction(zone)
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
      if (config.value.clickAction === 'auto') prevChapter(true)
    } else {
      container.scrollBy({ top: -delta, behavior: 'smooth' })
    }
  }
}

function handleScroll() {
  hideSelectionMenu()
  closeDiffPreview()
  const container = scrollContainerRef.value
  if (container && isContinuousMode.value && continuousChapters.value.length) {
    if (isContinuousScrollSyncSuppressed()) {
      scheduleSaveReadingPosition()
      return
    }
    const sections = getContinuousSections()
    if (sections.length) {
      const anchorLine =
        container.scrollTop + container.clientHeight * CONTINUOUS_POSITION_ANCHOR_RATIO
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
        (nextSection ? nextSection.offsetTop : container.scrollHeight) - activeSection.offsetTop
      )
      const progress = Math.max(
        0,
        Math.min(1, (container.scrollTop - activeSection.offsetTop) / sectionRange)
      )
      if (activeChapter) {
        if (activeChapter.prunedHeight != null) {
          // 裁剪占位章进入锚线：回载真实内容，绝不同步它的空 content 进 store
          void rehydrateContinuousChapter(activeIndex)
        } else if (store.currentIndex !== activeIndex || store.content !== activeChapter.content) {
          setContinuousActiveChapter(activeIndex, activeChapter.content, progress)
        } else {
          store.setChapterScrollProgress(progress)
        }
      }
    }

    if (
      !isContinuousAutoLoadSuppressed() &&
      container.scrollHeight - (container.scrollTop + container.clientHeight) < 480
    ) {
      loadContinuousNext()
    }
  } else if (container && isHorizontalPageMode.value) {
    // 横翻的滚动事件只可能来自 resize/对齐抖动（页面由 transform 驱动）：
    // 归位后复用翻页时那套进度/末页态/预加载/保存/上报链路，不再平行写第二遍
    updateHorizontalMetrics()
    const maxPage = Math.max(0, horizontalPages.value.length - 1)
    horizontalPageIndex.value = Math.max(0, Math.min(maxPage, horizontalPageIndex.value))
    if (container.scrollLeft !== 0) {
      container.scrollTo({ left: 0, behavior: 'auto' })
    }
    if (showControls.value && !store.activePanel) {
      showControls.value = false
    }
    syncHorizontalPageState()
    return
  } else if (container) {
    const maxScroll = Math.max(1, container.scrollHeight - container.clientHeight)
    const progress =
      container.scrollHeight <= container.clientHeight ? 1 : container.scrollTop / maxScroll
    store.setChapterScrollProgress(progress)
    if (
      config.value.enablePreload &&
      container.scrollHeight - (container.scrollTop + container.clientHeight) <
        container.clientHeight * 1.5
    ) {
      store.preloadAroundChapter(store.currentIndex)
    }
  }
  if (showControls.value && !store.activePanel) {
    showControls.value = false
  }
  scheduleSaveReadingPosition()
  serverProgressAutoSaveScheduler.schedule()
}

/** 划线长按菜单：起点落在划线上时起 500ms 定时器，移动/抬起即取消 */
let underlineLongPressTimer: number | null = null

function cancelUnderlineLongPress() {
  if (underlineLongPressTimer) {
    clearTimeout(underlineLongPressTimer)
    underlineLongPressTimer = null
  }
}

function handleTouchStart(event: TouchEvent) {
  interruptAutoScroll()
  hideSelectionMenu()
  closeUnderlineMenu()
  cancelUnderlineLongPress()
  const touch = event.touches[0]
  if (!touch) return
  touchState.value = {
    startX: touch.clientX,
    startY: touch.clientY,
    startAt: Date.now(),
    moving: true,
    horizontalLocked: false,
  }
  const underline =
    config.value.selectAction !== 'ignore'
      ? resolveUnderlineTarget(event.target as HTMLElement | null)
      : null
  if (underline) {
    // 移动端没有右键通道：长按是触屏上打开划线菜单的方式，
    // 「忽略」模式下划线只读不拦截
    const { clientX, clientY } = touch
    underlineLongPressTimer = window.setTimeout(() => {
      underlineLongPressTimer = null
      // 触发的这次按压不再走翻页/点按链
      suppressNextTapUntil = Date.now() + 900
      touchState.value.moving = false
      openUnderlineMenu(
        clientX,
        clientY,
        underline.text,
        underline.chapterIndex,
        underline.chapterPos
      )
    }, 500)
  }
}

function handleTouchMove(event: TouchEvent) {
  const pressTouch = event.touches[0]
  if (pressTouch && underlineLongPressTimer) {
    const moved = Math.hypot(
      pressTouch.clientX - touchState.value.startX,
      pressTouch.clientY - touchState.value.startY
    )
    if (moved > 10) cancelUnderlineLongPress()
  }
  if (!isMobile.value || !isHorizontalPageMode.value || !touchState.value.moving) return
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
  cancelUnderlineLongPress()
  if (!isMobile.value || !isHorizontalPageMode.value || !touchState.value.moving) {
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
        const zone = resolveReaderTapZone(
          touch.clientX,
          touch.clientY,
          true,
          window.innerWidth,
          window.innerHeight
        )
        if (zone) clickZoneAction(zone)
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
    appStore.showToast('已恢复上次阅读的书籍', 'success')
  }
  loadSavedReadingPosition()
  window.addEventListener('keydown', handleKeydown)
  window.addEventListener('popstate', handleOverlayPopState)
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
  await Promise.all([store.fetchBookmarks(), store.fetchReplaceRules()])
  // 关键词高亮配置：本地镜像已就绪（store 初始化即读），后端合并不挡渲染
  void store.syncKeywordRules()
  // 已编辑章清单：目录徽章、预加载绕过与 diff 标记都靠它
  void store.refreshEditedChapterKeys()
  // 换源入口需要按启用书源数决定是否展示；会话内只拉一次（source store 缓存）
  if (!sourceStore.sources.length) {
    void sourceStore.fetchSources().catch(() => undefined)
  }
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
  window.removeEventListener('popstate', handleOverlayPopState)
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
  disposeOfflineBanner()
  if (rebuildHorizontalTimer) clearTimeout(rebuildHorizontalTimer)
  if (searchHighlightTimer) clearTimeout(searchHighlightTimer)
  disposeReadingPosition()
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

/* ─── watch 群：每条链注释自己的存在理由 ───
 * 扇出最重的两条：store.content（段落重置 → 连续模式就地改/重建 → 自动播放 →
 * 搜索重跑 → 离线横幅 → 位置恢复）与 store.currentIndex（读档 → 重置 → 裁剪 →
 * 预加载 → 连续同步 → 离线横幅 → 位置恢复）。改这两条链时先数清扇出。
 * 注意 watchers 各自独立触发，合并会改变触发时序——不要为省行数合并。
 */

watch(
  () => config.value.autoPageMode,
  () => {
    if (!store.isAutoScrolling) return
    stopAutoScroll()
    store.isAutoScrolling = true
    startAutoScroll()
  }
)

watch(
  () => config.value.readMethod,
  async () => {
    // 切换前记下当前章内进度（竖滚/横翻都在持续维护 chapterScrollProgress），
    // 交给结尾的 scheduleRestoreReadingPosition 在新模式下恢复，而不是回章首
    if (store.book && store.chapterScrollProgress > 0) {
      queueRestorePosition({
        chapterIndex: store.currentIndex,
        progress: store.chapterScrollProgress,
        updatedAt: Date.now(),
      })
    }
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
  }
)

// 段评计数与章节配图是异步到达的装饰层：竖滚模式由 v-html 自动跟随，
// 横翻模式的分页快照需要主动重排。不重置页码——rebuildHorizontalPages 内部只 clamp 不归零。
watch(formattedContent, () => {
  if (isHorizontalPageMode.value) scheduleRebuildHorizontalPages()
})

watch(
  () => store.currentIndex,
  () => {
    if (!isHorizontalPageMode.value) return
    resetHorizontalPagePosition()
    scheduleRebuildHorizontalPages()
  }
)

watch(
  [
    () => store.content,
    () => config.value.fontSize,
    () => config.value.fontWeight,
    () => config.value.lineHeight,
    () => config.value.paragraphSpacing,
    () => config.value.firstLineIndent,
    () => config.value.showChapterImages,
    // showSearch/searchQuery 曾是依赖：开搜索面板、每敲一个字都会把横翻页码
    // 归零丢回本章首页。搜索高亮已改成渲染后 DOM 级标记（applySearchHighlightToDom），
    // 不进 formattedContent 依赖链，这里不需要也不该归零
  ],
  () => {
    if (isHorizontalPageMode.value) {
      horizontalPageIndex.value = 0
      scheduleRebuildHorizontalPages()
    }
  }
)

// 页面宽度/双页开关决定横翻列宽：变化时重排并按当前章内进度恢复，
// 不打断阅读位置（与切换翻页方式同一套恢复通道）
watch([() => config.value.pageWidth, isDualPageMode], () => {
  if (!isHorizontalPageMode.value) return
  if (store.book && store.chapterScrollProgress > 0) {
    queueRestorePosition({
      chapterIndex: store.currentIndex,
      progress: store.chapterScrollProgress,
      updatedAt: Date.now(),
    })
  }
  scheduleRebuildHorizontalPages()
  scheduleRestoreReadingPosition()
})

watch(
  () => store.currentIndex,
  async () => {
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
  }
)

watch(
  [() => store.chapters.length, () => store.chaptersLoading, () => store.loading, isContinuousMode],
  async ([chapterCount, chaptersLoading, loadingNow, continuousMode]) => {
    if (
      !continuousMode ||
      !chapterCount ||
      chaptersLoading ||
      loadingNow ||
      continuousChapters.value.length
    )
      return
    await initializeContinuousChapters(store.currentIndex, false)
    scheduleRestoreReadingPosition()
  },
  { immediate: true }
)

watch(
  () => store.content,
  () => {
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
  }
)

// 配图比正文晚到：连续滚动模式的 HTML 是预渲染的，得为当前章补一次
watch(
  () => store.chapterImages,
  () => {
    if (!isContinuousMode.value) return
    const current = getContinuousChapter(store.currentIndex)
    if (current) current.html = renderChapterHtml(store.content, current.index)
  }
)

watch(
  () => store.loading,
  (loading) => {
    if (!loading && pendingRestorePosition.value) {
      scheduleRestoreReadingPosition()
    }
  }
)

watch(
  () => store.book?.bookId,
  () => {
    loadSavedReadingPosition()
    scheduleRefreshOfflineCacheState()
    void store.refreshEditedChapterKeys()
  }
)

// 切换编辑版/上游原版：重新取当前章（连续模式连同视口内的邻章一起重建）
watch(
  () => config.value.contentVersion,
  async () => {
    if (isContinuousMode.value) {
      await rebuildContinuousAtChapter(store.currentIndex)
      void store.loadChapterReviews(store.currentIndex)
    } else {
      await store.reloadChapterAfterEdit(store.currentIndex)
    }
  }
)

watch(
  [
    () => config.value.paragraphSpacing,
    () => config.value.firstLineIndent,
    () => config.value.showChapterImages,
    () => config.value.chineseMode,
    () => store.replaceRules,
    // 常驻关键词高亮配置变化：重跑装饰层上色
    () => store.keywordRules,
    // 划线增删：回显的摘录文本集合变了要重跑装饰层
    () => store.bookmarks,
    // 夜间切换：关键词底色要换成暗色变体（连续模式章缓存要刷）
    () => store.isNight,
    // diff 标记开关：关掉时重跑装饰层摘掉标记
    () => config.value.showDiffMarks,
  ],
  () => {
    if (isContinuousMode.value) {
      syncContinuousChapterHtml()
    }
    handlePresentationUpdated()
  }
)

/* ─── 搜索高亮：渲染后 DOM 级标记 ───
 * 不进 formattedContent 依赖链（否则搜索框每敲一个字都重建整章 DOM，
 * 横翻模式还会级联重排）。输入防抖 250ms 后直接在已渲染 DOM 上
 * 清旧 mark、打新 mark；正文重渲染（v-html 重建）后再补回。
 * 模板里竖滚/横翻的正文容器已有 chapterTextRef，但连续模式每章一个
 * 正文根，统一按滚动容器内的 .chapter-text 收集，三种模式一套代码。 */
function searchHighlightRoots(): HTMLElement[] {
  const container = scrollContainerRef.value
  if (!container) return []
  return Array.from(container.querySelectorAll('.chapter-text')) as HTMLElement[]
}

/** 只摘搜索高亮的 mark（mark.search-highlight）：划线 span、diff 标记、
 *  段评气泡与常驻关键词 mark.kw-highlight 都原样保留。unwrap 后 normalize
 *  合并相邻文本节点，恢复成单文本节点，下次高亮才能跨原标记边界匹配 */
function clearSearchHighlightMarks() {
  for (const root of searchHighlightRoots()) {
    root.querySelectorAll('mark.search-highlight').forEach((mark) => {
      const parent = mark.parentNode
      if (!parent) return
      mark.replaceWith(...Array.from(mark.childNodes))
      parent.normalize()
    })
  }
}

function applySearchHighlightToDom() {
  clearSearchHighlightMarks()
  const query = showSearch.value ? searchQuery.value : ''
  if (!query) return
  for (const root of searchHighlightRoots()) {
    highlightSearchText(root, query)
  }
}

let searchHighlightTimer: number | null = null

watch([searchQuery, showSearch], () => {
  if (searchHighlightTimer) {
    clearTimeout(searchHighlightTimer)
    searchHighlightTimer = null
  }
  if (!showSearch.value) {
    // 关闭面板立即清干净，不留 250ms 残影
    applySearchHighlightToDom()
    return
  }
  searchHighlightTimer = window.setTimeout(() => {
    searchHighlightTimer = null
    applySearchHighlightToDom()
    // 结果集刷新与命中定位沿用原链路（打字时面板结果跟着变）；
    // 放在标记之后，scrollToMatch 才能找到新 mark
    handlePresentationUpdated()
  }, 250)
})

// 正文重渲染后补回搜索高亮：切章/强刷/装饰层重跑（竖滚 + 横翻重排）与
// 连续模式章 html 重建/占位回载都会让 v-html 生成全新 DOM。只往 DOM 打
// mark、不回写任何响应式状态，不会反过来再触发 formattedContent 或横翻重排
watch([formattedContent, horizontalPages], async () => {
  await nextTick()
  applySearchHighlightToDom()
})

watch(
  continuousChapters,
  async () => {
    await nextTick()
    applySearchHighlightToDom()
  },
  { deep: true }
)

watch(
  () => config.value.selectAction,
  (value) => {
    if (value !== 'popup') {
      clearSelectionState()
    }
  }
)

watch(
  () => store.isSpeaking,
  (speaking) => {
    if (speaking && !ttsPanelDismissed.value) {
      showTTSPanel.value = true
    }
    if (!speaking && !store.isAutoScrolling) {
      clearReadingClass()
    }
  }
)

watch(
  [() => store.isNight, () => store.currentTheme.body, () => appStore.theme],
  ([isNight, body]) => {
    applySystemTheme(isNight ? 'dark' : appStore.theme, body)
  },
  { immediate: true }
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
  transition:
    background 0.3s,
    color 0.3s;
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
  background: rgba(0, 0, 0, 0.1);
  border-radius: 4px;
}
.reader-view[style*='background: #1a1a2e'] .reader-scroll-container::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.1);
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
  border: 3px solid rgba(0, 0, 0, 0.1);
  border-top-color: var(--color-primary);
  border-radius: 50%;
  animation: spin 1s linear infinite;
}
.reader-view[style*='background: #1a1a2e'] .loading-spinner {
  border-color: rgba(255, 255, 255, 0.1);
  border-top-color: var(--color-primary);
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
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
  width: 100%;
  padding: 0;
}

/* 横翻的可见窗口：宽度 = 单页(单列)或双页(两列)，居中显示。
   窗口必须 overflow:hidden 裁剪页条——窗口比容器窄时，已翻过的页
   会露在两侧留白里 */
.horizontal-page-layout {
  width: var(--reader-page-window);
  max-width: 100%;
  margin: 0 auto;
  height: 100%;
}

.horizontal-content-page {
  width: 100%;
  height: 100%;
  min-height: 100%;
  padding: 0;
  box-sizing: border-box;
  overflow: hidden;
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

/* 双页模式：右列（偶数页）左缘加一条浅分隔线 */
.dual-page-mode .horizontal-page:nth-child(even) {
  border-left: 1px solid rgba(128, 128, 128, 0.18);
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

/* 上部裁剪的占位章：只留一个淡标题撑住高度，回到视口附近即回载真实内容 */
.chapter-placeholder {
  color: var(--color-text-secondary, #999);
  font-size: var(--text-sm);
  opacity: 0.5;
  user-select: none;
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
  display: flex;
  align-items: center;
  gap: 10px;
}

.horizontal-next-floating .next-btn,
.horizontal-next-floating .horizontal-comments-entry {
  pointer-events: auto;
  background: rgba(255, 255, 255, 0.75);
  backdrop-filter: blur(6px);
}

/* 横翻章末浮层里的评论入口：窄条形态，区别于竖滚的整行 bar */
.horizontal-next-floating .horizontal-comments-entry {
  width: auto;
  margin-top: 0;
  padding: 8px 14px;
  border-radius: 30px;
  box-shadow: 0 2px 12px rgba(0, 0, 0, 0.12);
}

/* 横翻页内图片限高：分页是定高裁剪，图片不能超过单页内容区，
   否则超出部分直接被 overflow:hidden 切掉 */
:deep(.horizontal-page-content img) {
  max-height: var(--reader-page-content-height, none);
  object-fit: contain;
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
  background: rgba(0, 0, 0, 0.05);
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
  box-shadow: 4px 0 24px rgba(0, 0, 0, 0.15);
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
  /* 2×2：加入划线 / 加入高亮 一行，按本书替换 / 按书源替换 一行 */
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

/* 划线菜单里的删除动作：红色与全站 danger 一致 */
.selection-menu-actions button.danger-action {
  background: #ef4444;
}

/* ─── diff 标记的就地预览卡 ─── */
.diff-preview {
  min-width: 240px;
  max-width: min(420px, calc(100vw - 32px));
}

.diff-preview-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px 4px;
  font-size: var(--text-sm);
  font-weight: 600;
}

.diff-preview-close {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  padding: 0;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: inherit;
  opacity: 0.55;
  cursor: pointer;
}

.diff-preview-close:hover {
  opacity: 1;
  background: var(--color-bg-hover);
}

.diff-preview-close svg {
  width: 14px;
  height: 14px;
}

.diff-preview-body {
  max-height: 40vh;
  overflow-y: auto;
  padding: 2px 14px 10px;
  font-size: var(--text-sm);
  line-height: 1.6;
}

.diff-preview-row {
  display: flex;
  gap: 8px;
  padding: 3px 0;
  white-space: pre-wrap;
  word-break: break-word;
}

.diff-preview-row.del {
  color: #ef4444;
}

.diff-preview-row.add {
  color: #16a34a;
}

.diff-preview-gutter {
  flex: none;
  width: 10px;
  opacity: 0.8;
  user-select: none;
  -webkit-user-select: none;
}

.diff-preview-ins {
  background: rgba(34, 197, 94, 0.22);
  border-radius: 2px;
}

.diff-preview-label {
  margin-top: 6px;
  padding-top: 6px;
  border-top: 1px dashed rgba(128, 128, 128, 0.3);
  font-size: var(--text-xs);
  opacity: 0.65;
}

.diff-preview-more {
  padding-top: 2px;
  font-size: var(--text-xs);
  opacity: 0.6;
}

.diff-preview-actions {
  display: flex;
  justify-content: flex-end;
  padding: 0 12px 12px;
}

.diff-preview-actions button {
  border: none;
  border-radius: 10px;
  padding: 8px 14px;
  background: var(--color-primary);
  color: #fff;
  font-size: var(--text-sm);
  cursor: pointer;
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

:deep(.search-highlight) {
  background: yellow;
  color: black;
  border-radius: 2px;
}

:deep(.search-highlight.current-match) {
  background: orange;
}

/* 常驻关键词标记：底色由内联 style 给（用户配置的色盘值），这里只管形状 */
:deep(.kw-highlight) {
  border-radius: 2px;
  color: inherit;
  padding: 0 1px;
}

/* 划线回显：主题色下划线，与关键词底色/搜索高亮可叠加；可点击打开操作菜单 */
:deep(.reader-underline) {
  border-bottom: 2px solid var(--color-primary, #c97f3a);
  padding-bottom: 1px;
  cursor: pointer;
}

/* 「忽略」模式下划线只读，不给可点暗示 */
.select-action-ignore :deep(.reader-underline) {
  cursor: default;
}

/* ─── 编辑版 diff 标记（vscode 风） ─── */
/* 新增段：左绿竖线。竖线用绝对定位，不占内容盒宽度——border/padding 会把
   被标记段落整体右移，正文首行缩进就和其他段对不齐了 */
:deep(.chapter-text p.diff-added),
:deep(.chapter-text p.diff-modified) {
  position: relative;
}

:deep(p.diff-added)::after,
:deep(p.diff-modified)::after {
  content: '';
  position: absolute;
  left: -12px;
  top: 0.15em;
  bottom: 0.15em;
  width: 3px;
  border-radius: 2px;
  pointer-events: none;
}

:deep(p.diff-added)::after {
  background: #22c55e;
}

:deep(p.diff-modified)::after {
  background: #3b82f6;
}

/* 段内相对原段增加的部分：虚线下划线 */
:deep(span.diff-ins) {
  text-decoration: underline dotted #22c55e;
  text-underline-offset: 3px;
}

/* 段内减少处：零宽红竖线（title 带被删原文）。
   竖线自身 2px 宽，用负 margin 抵掉，文字位置不因标记而移动 */
:deep(span.diff-del-marker) {
  display: inline-block;
  width: 0;
  height: 1em;
  border-left: 2px solid #ef4444;
  margin-left: -2px;
  vertical-align: -0.15em;
}

/* 大段删除：删除块后第一个未变段的左上角红箭头（绝对定位零占位，不影响分页测量） */
:deep(p.diff-del-after) {
  position: relative;
}

:deep(p.diff-del-after)::before {
  content: '◀';
  position: absolute;
  left: -18px;
  top: 0.15em;
  color: #ef4444;
  font-size: 0.8em;
  line-height: 1;
  pointer-events: none;
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

  /* 移动端字号本来就大，章节标题收一档 */
  .chapter-title {
    font-size: 1.35em;
    margin-bottom: 0.9em;
  }

  :deep(.horizontal-page-content .horizontal-flow-title) {
    font-size: 1.25em;
    margin-bottom: 0.8em;
  }

  .continuous-reading {
    padding: 16px 0 8px;
  }

  .continuous-chapter {
    padding-top: 20px;
    padding-bottom: 8px;
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
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.3s;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

.slide-left-enter-active,
.slide-left-leave-active {
  transition: transform 0.35s cubic-bezier(0.2, 0.8, 0.2, 1);
}
.slide-left-enter-from,
.slide-left-leave-to {
  transform: translateX(-100%);
}
</style>
