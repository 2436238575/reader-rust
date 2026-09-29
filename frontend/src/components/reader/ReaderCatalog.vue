<template>
  <div class="reader-catalog" :style="{ background: theme.popup, color: theme.fontColor }">
    <div class="catalog-header">
      <div class="tabs">
        <div
          class="tab"
          :class="{ active: activeTab === 'chapters' }"
          @click="activeTab = 'chapters'"
        >
          目录
        </div>
        <div
          class="tab"
          :class="{ active: activeTab === 'bookmarks' }"
          @click="activeTab = 'bookmarks'"
        >
          书签
        </div>
      </div>
      <div class="header-actions">
        <button
          v-if="activeTab === 'chapters'"
          class="icon-btn"
          :disabled="store.chaptersLoading"
          @click="refreshCatalog"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M21 12a9 9 0 1 1-2.64-6.36M21 3v6h-6" />
          </svg>
        </button>
        <button class="close-btn" @click="store.closePanel()">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        </button>
      </div>
    </div>

    <div v-if="activeTab === 'bookmarks'" class="bookmark-toolbar">
      <button class="bookmark-action primary" @click="addCurrentBookmark">添加当前页书签</button>
      <button
        class="bookmark-action"
        :class="{ danger: bookmarkEditMode && selectedBookmarkKeys.size > 0 }"
        @click="handleBatchAction"
      >
        {{
          bookmarkEditMode
            ? selectedBookmarkKeys.size
              ? `删除选中(${selectedBookmarkKeys.size})`
              : '完成'
            : '批量管理'
        }}
      </button>
    </div>

    <div v-show="activeTab === 'chapters'" class="chapter-toolbar">
      <div class="search-box">
        <input
          v-model="chapterSearchInput"
          type="text"
          placeholder="搜索章节..."
          class="search-input"
        />
        <button
          v-if="chapterSearchInput"
          class="search-clear"
          @click="clearChapterSearch"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        </button>
      </div>
      <div class="chapter-jump-actions">
        <button
          class="jump-btn"
          title="跳到目录顶部"
          :disabled="!filteredChapters.length"
          @click="scrollCatalogTo('top')"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="m18 15-6-6-6 6" />
            <path d="M5 5h14" />
          </svg>
        </button>
        <button
          class="jump-btn"
          title="定位当前章节"
          :disabled="!currentChapterInList"
          @click="scrollToCurrent"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M12 2v3" />
            <path d="M12 19v3" />
            <path d="M2 12h3" />
            <path d="M19 12h3" />
            <circle cx="12" cy="12" r="3" />
          </svg>
        </button>
        <button
          class="jump-btn"
          title="跳到目录底部"
          :disabled="!filteredChapters.length"
          @click="scrollCatalogTo('bottom')"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="m6 9 6 6 6-6" />
            <path d="M5 19h14" />
          </svg>
        </button>
      </div>
    </div>

    <!-- Chapters List -->
    <div v-show="activeTab === 'chapters'" class="list-container" ref="listRef">
      <div v-if="store.chaptersLoading" class="loading">加载目录中...</div>
      <div v-else-if="filteredChapters.length === 0" class="empty">未找到匹配的章节</div>
      <div
        v-else
        v-for="chapter in filteredChapters"
        :key="chapter.index"
        class="list-item"
        :class="{
          active: chapter.index === store.currentIndex,
          read: store.isChapterRead(chapter.index),
        }"
        @click="goToChapter(chapter.index)"
      >
        <span class="item-index">{{ chapter.index + 1 }}</span>
        <span class="item-title">{{ chapter.title }}</span>
        <div class="item-status">
          <span v-if="chapter.index === store.currentIndex" class="status-badge current">当前</span>
          <span v-else-if="store.isChapterRead(chapter.index)" class="status-badge read">已读</span>
          <span v-if="isChapterCached(chapter.url)" class="status-badge cached">已缓存</span>
        </div>
      </div>
    </div>

    <!-- Bookmarks List -->
    <div v-show="activeTab === 'bookmarks'" class="list-container">
      <div v-if="!store.bookmarks.length" class="empty">暂无书签</div>
      <div
        v-else
        v-for="(bm, idx) in store.bookmarks"
        :key="idx"
        class="list-item bookmark-item"
        :class="{ selected: isBookmarkSelected(bm) }"
        @click="goToBookmark(bm)"
      >
        <button
          v-if="bookmarkEditMode"
          class="bookmark-check"
          :class="{ checked: isBookmarkSelected(bm) }"
          @click.stop="toggleBookmarkSelection(bm)"
        >
          ✓
        </button>
        <div class="bm-header">
          <span class="bm-chapter">{{ bm.chapterName }}</span>
          <span class="bm-time">{{ formatDate(bm.time) }}</span>
        </div>
        <div class="bm-snippet">{{ bm.bookText }}</div>
        <button v-if="!bookmarkEditMode" class="bm-delete" @click.stop="store.removeBookmark(bm)">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path
              d="M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"
            />
          </svg>
        </button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, nextTick, watch } from 'vue'
import { useReaderStore } from '../../stores/reader'
import { useAppStore } from '../../stores/app'
import type { Bookmark } from '../../types'
import { listBrowserCachedChapterUrls } from '../../utils/browserCache'
import { isLocalBook } from '../../utils/localBook'
import { formatMonthDayTime } from '../../utils/format'

const props = withDefaults(
  defineProps<{
    initialTab?: 'chapters' | 'bookmarks'
  }>(),
  {
    initialTab: 'chapters',
  }
)
const emit = defineEmits<{
  jumpChapter: [index: number]
  jumpBookmark: [bookmark: Bookmark]
}>()

const store = useReaderStore()
const appStore = useAppStore()
const theme = computed(() => store.currentTheme)
const activeTab = ref<'chapters' | 'bookmarks'>(props.initialTab)
const listRef = ref<HTMLElement>()
const bookmarkEditMode = ref(false)
const selectedBookmarkKeys = ref<Set<string>>(new Set())
const cachedChapterUrls = ref<Set<string>>(new Set())
const chapterSearchInput = ref('')
// 目录可上千章：每个键都全量 filter + 重渲染太贵，200ms 防抖后再进 computed
const chapterSearch = ref('')
let chapterSearchTimer: number | null = null
function clearChapterSearch() {
  chapterSearchInput.value = ''
  chapterSearch.value = ''
}
watch(chapterSearchInput, (value) => {
  if (chapterSearchTimer) clearTimeout(chapterSearchTimer)
  chapterSearchTimer = window.setTimeout(() => {
    chapterSearch.value = value
  }, 200)
})

// Filtered chapters based on search
const filteredChapters = computed(() => {
  const chapters = store.chapters
  const searchTerm = chapterSearch.value.toLowerCase().trim()
  if (!searchTerm) {
    // 无搜索词时直接复用原数组：此前每次重算都把整目录复制成新对象
    //（千章 = 数千个新对象，触发全列表重新 diff）
    return chapters.every((chapter, index) => chapter.index === index)
      ? chapters
      : chapters.map((chapter, index) => ({ ...chapter, index }))
  }
  return chapters
    .map((chapter, index) => ({ ...chapter, index }))
    .filter((chapter) => chapter.title.toLowerCase().includes(searchTerm))
})

// 搜索过滤后当前章可能不在列表里，此时定位按钮不可点
const currentChapterInList = computed(() =>
  filteredChapters.value.some((chapter) => chapter.index === store.currentIndex)
)

onMounted(() => {
  activeTab.value = props.initialTab
  scrollToCurrent()
  store.fetchBookmarks()
  void refreshCachedChapterState()
})

watch(
  () => props.initialTab,
  (tab) => {
    activeTab.value = tab
    if (tab !== 'bookmarks') {
      bookmarkEditMode.value = false
      selectedBookmarkKeys.value.clear()
    }
    if (tab === 'chapters') {
      void refreshCachedChapterState()
    }
  }
)

watch(
  () => store.book?.bookUrl,
  () => {
    void refreshCachedChapterState()
  }
)

watch(
  () => store.chapters,
  () => {
    void refreshCachedChapterState()
  }
)

function scrollToCurrent() {
  // 长目录渲染需要时间（content-visibility 懒绘制）：一次 nextTick 可能还找不到
  // 当前章，补一次 250ms 重试兜底
  nextTick(() => {
    const activeEl = listRef.value?.querySelector('.list-item.active')
    if (activeEl) {
      activeEl.scrollIntoView({ block: 'center' })
      return
    }
    window.setTimeout(() => {
      listRef.value?.querySelector('.list-item.active')?.scrollIntoView({ block: 'center' })
    }, 250)
  })
}

function scrollCatalogTo(position: 'top' | 'bottom') {
  const el = listRef.value
  if (!el) return
  el.scrollTo({
    top: position === 'top' ? 0 : el.scrollHeight,
    behavior: 'smooth',
  })
}

async function goToChapter(index: number) {
  emit('jumpChapter', index)
}

async function refreshCatalog() {
  await store.refreshChapters()
  await refreshCachedChapterState()
  scrollToCurrent()
}

async function refreshCachedChapterState() {
  if (!store.book || isLocalBook(store.book) || !store.chapters.length) {
    cachedChapterUrls.value = new Set()
    return
  }
  cachedChapterUrls.value = await listBrowserCachedChapterUrls(store.book.bookUrl).catch(
    () => new Set()
  )
}

function isChapterCached(chapterUrl?: string) {
  return !!chapterUrl && cachedChapterUrls.value.has(chapterUrl)
}

async function goToBookmark(bm: Bookmark) {
  if (bookmarkEditMode.value) {
    toggleBookmarkSelection(bm)
    return
  }
  if (bm.chapterIndex !== undefined) {
    // chapterPos 是收藏时的章内进度：交给阅读器走位置恢复，而不是落在章首
    emit('jumpBookmark', bm)
  }
}

function getBookmarkKey(bm: Bookmark) {
  return `${bm.bookName}|${bm.bookAuthor}|${bm.chapterIndex}|${bm.chapterPos}|${bm.time}|${bm.bookText}`
}

function isBookmarkSelected(bm: Bookmark) {
  return selectedBookmarkKeys.value.has(getBookmarkKey(bm))
}

function toggleBookmarkSelection(bm: Bookmark) {
  const key = getBookmarkKey(bm)
  if (selectedBookmarkKeys.value.has(key)) {
    selectedBookmarkKeys.value.delete(key)
  } else {
    selectedBookmarkKeys.value.add(key)
  }
}

async function addCurrentBookmark() {
  try {
    await store.addBookmark()
    appStore.showToast('已添加当前页书签', 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || '添加书签失败', 'error')
  }
}

async function handleBatchAction() {
  if (!bookmarkEditMode.value) {
    bookmarkEditMode.value = true
    return
  }
  if (!selectedBookmarkKeys.value.size) {
    bookmarkEditMode.value = false
    return
  }
  const items = store.bookmarks.filter((bookmark) =>
    selectedBookmarkKeys.value.has(getBookmarkKey(bookmark))
  )
  if (!confirm(`确定删除选中的 ${items.length} 条书签吗？`)) return
  try {
    await store.removeBookmarks(items)
    selectedBookmarkKeys.value.clear()
    bookmarkEditMode.value = false
    appStore.showToast(`已删除 ${items.length} 条书签`, 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || '删除书签失败', 'error')
  }
}

const formatDate = formatMonthDayTime
</script>

<style scoped>
.reader-catalog {
  display: flex;
  flex-direction: column;
  height: 100%;
}

.catalog-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 12px;
  border-bottom: 1px solid rgba(0, 0, 0, 0.06);
  flex-shrink: 0;
  height: 56px;
}

.tabs {
  display: flex;
  gap: 20px;
  height: 100%;
}

.tab {
  height: 100%;
  display: flex;
  align-items: center;
  font-size: var(--text-base);
  font-weight: 500;
  cursor: pointer;
  opacity: 0.6;
  border-bottom: 2px solid transparent;
  padding: 0 4px;
}

.tab.active {
  opacity: 1;
  color: var(--color-primary, #c97f3a);
  border-bottom-color: var(--color-primary, #c97f3a);
}

.icon-btn,
.close-btn {
  width: 32px;
  height: 32px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 8px;
  color: inherit;
  opacity: 0.6;
  background: transparent;
  border: none;
  cursor: pointer;
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.icon-btn:disabled {
  opacity: 0.35;
  cursor: default;
}

.list-container {
  flex: 1;
  overflow-y: auto;
  padding: 4px 0 8px;
  -webkit-overflow-scrolling: touch;
  overscroll-behavior: contain;
}

.chapter-toolbar {
  flex-shrink: 0;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 16px;
  border-bottom: 1px solid rgba(0, 0, 0, 0.06);
}

.search-box {
  flex: 1;
  min-width: 0;
  position: relative;
}

.search-input {
  width: 100%;
  padding: 8px 32px 8px 12px;
  border: 1px solid rgba(0, 0, 0, 0.1);
  border-radius: 8px;
  background: rgba(0, 0, 0, 0.03);
  color: inherit;
  font-size: var(--text-base);
  outline: none;
  transition: all 0.2s;
}

.search-input:focus {
  border-color: var(--color-primary, #c97f3a);
  background: rgba(0, 0, 0, 0.02);
}

.search-clear {
  position: absolute;
  right: 8px;
  top: 50%;
  transform: translateY(-50%);
  width: 20px;
  height: 20px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  background: rgba(0, 0, 0, 0.1);
  color: inherit;
  border: none;
  cursor: pointer;
  opacity: 0.6;
  transition: opacity 0.2s;
}

.search-clear:hover {
  opacity: 1;
}

.search-clear svg {
  width: 12px;
  height: 12px;
}

.chapter-jump-actions {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  flex: 0 0 auto;
}

.jump-btn {
  width: 36px;
  height: 36px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 8px;
  border: 1px solid rgba(0, 0, 0, 0.08);
  background: rgba(0, 0, 0, 0.025);
  color: inherit;
  opacity: 0.7;
  cursor: pointer;
  transition: all 0.2s;
}

.jump-btn:hover:not(:disabled) {
  opacity: 1;
  border-color: rgba(201, 127, 58, 0.35);
  background: rgba(201, 127, 58, 0.08);
  color: var(--color-primary, #c97f3a);
}

.jump-btn:disabled {
  opacity: 0.28;
  cursor: default;
}

.jump-btn svg {
  width: 17px;
  height: 17px;
}

.bookmark-toolbar {
  display: flex;
  gap: 10px;
  padding: 12px 16px 0;
  flex-wrap: wrap;
}

.bookmark-action {
  border: 1px solid rgba(0, 0, 0, 0.08);
  border-radius: 999px;
  background: transparent;
  color: inherit;
  padding: 8px 12px;
  font-size: var(--text-xs);
  cursor: pointer;
}

.bookmark-action.primary {
  border-color: var(--color-primary, #c97f3a);
  color: var(--color-primary, #c97f3a);
}

.bookmark-action.danger {
  border-color: #ef4444;
  color: #ef4444;
}

.loading,
.empty {
  padding: 40px;
  text-align: center;
  opacity: 0.5;
  font-size: var(--text-base);
}

.list-item {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 20px;
  cursor: pointer;
  /* 千章目录：视口外的行跳过布局与绘制 */
  content-visibility: auto;
  contain-intrinsic-size: auto 46px;
  transition: all 0.2s;
  border-bottom: 1px solid rgba(0, 0, 0, 0.02);
}

.list-item:hover {
  background: rgba(0, 0, 0, 0.03);
}

.list-item.active {
  color: var(--color-primary, #c97f3a);
  background: rgba(201, 127, 58, 0.05);
}

.list-item.read:not(.active) .item-title {
  opacity: 0.74;
}

.item-index {
  font-size: var(--text-xs);
  opacity: 0.4;
  width: 24px;
  flex-shrink: 0;
}

.item-title {
  font-size: var(--text-base);
  line-height: 1.4;
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.item-status {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 6px;
  flex-shrink: 0;
}

.status-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  padding: 2px 8px;
  border-radius: 999px;
  font-size: var(--text-xs);
  line-height: 1.4;
  white-space: nowrap;
  border: 1px solid transparent;
}

.status-badge.current {
  color: var(--color-primary, #c97f3a);
  background: rgba(201, 127, 58, 0.12);
  border-color: rgba(201, 127, 58, 0.2);
}

.status-badge.read {
  color: rgba(0, 0, 0, 0.5);
  background: rgba(0, 0, 0, 0.05);
}

.status-badge.cached {
  color: #2563eb;
  background: rgba(37, 99, 235, 0.1);
  border-color: rgba(37, 99, 235, 0.14);
}

/* Bookmark items */
.bookmark-item {
  flex-direction: column;
  gap: 6px;
  position: relative;
  padding-left: 48px;
}

.bookmark-item.selected {
  background: rgba(201, 127, 58, 0.08);
}

.bookmark-check {
  position: absolute;
  left: 16px;
  top: 16px;
  width: 22px;
  height: 22px;
  border-radius: 50%;
  border: 1px solid rgba(0, 0, 0, 0.15);
  background: transparent;
  color: transparent;
  cursor: pointer;
}

.bookmark-check.checked {
  background: var(--color-primary, #c97f3a);
  border-color: var(--color-primary, #c97f3a);
  color: #fff;
}

.bm-header {
  width: 100%;
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.bm-chapter {
  font-size: var(--text-sm);
  font-weight: 600;
  opacity: 0.9;
}

.bm-time {
  font-size: var(--text-xs);
  opacity: 0.4;
}

.bm-snippet {
  font-size: var(--text-xs);
  opacity: 0.6;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  line-height: 1.5;
}

.bm-delete {
  position: absolute;
  right: 12px;
  bottom: 12px;
  width: 28px;
  height: 28px;
  display: flex;
  align-items: center;
  justify-content: center;
  opacity: 0;
  background: rgba(255, 0, 0, 0.05);
  color: #ef4444;
  border: none;
  border-radius: 4px;
  cursor: pointer;
  transition: 0.2s;
}

.bookmark-item:hover .bm-delete {
  opacity: 1;
}

.bm-delete svg {
  width: 14px;
  height: 14px;
}
</style>
