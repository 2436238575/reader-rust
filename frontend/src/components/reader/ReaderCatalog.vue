<template>
  <div class="reader-catalog" :style="{ background: theme.popup, color: theme.fontColor }">
    <div class="catalog-header">
      <div class="tabs">
        <div
          class="tab"
          :class="{ active: activeTab === 'chapters' }"
          @click="switchTab('chapters')"
        >
          目录
        </div>
        <div
          class="tab"
          :class="{ active: activeTab === 'bookmarks' }"
          @click="switchTab('bookmarks')"
        >
          书签
        </div>
        <div
          class="tab"
          :class="{ active: activeTab === 'underlines' }"
          @click="switchTab('underlines')"
        >
          划线
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

    <div v-if="activeTab !== 'chapters'" class="bookmark-toolbar">
      <button
        v-if="activeTab === 'bookmarks'"
        class="bookmark-action primary"
        @click="addCurrentBookmark"
      >
        添加当前页书签
      </button>
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
        <button v-if="chapterSearchInput" class="search-clear" @click="clearChapterSearch">
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
      <template v-else>
        <template v-for="row in catalogRows" :key="row.key">
          <div v-if="row.kind === 'volume'" class="volume-header">{{ row.name }}</div>
          <div
            v-else
            class="list-item"
            :class="{
              active: row.chapter.index === store.currentIndex,
              read: store.isChapterRead(row.chapter.index),
              'with-meta': !!row.meta,
            }"
            @click="goToChapter(row.chapter.index)"
          >
            <span class="item-index">{{ row.chapter.index + 1 }}</span>
            <div class="item-main">
              <span class="item-title">{{ row.chapter.title }}</span>
              <span v-if="row.meta" class="item-meta">{{ row.meta }}</span>
            </div>
            <div class="item-status">
              <span v-if="row.chapter.index === store.currentIndex" class="status-badge current"
                >当前</span
              >
              <span v-if="isChapterCached(row.chapter.url)" class="status-badge cached"
                >已缓存</span
              >
            </div>
          </div>
        </template>
      </template>
    </div>

    <!-- Bookmarks List -->
    <div v-show="activeTab === 'bookmarks'" class="list-container">
      <div v-if="!chapterBookmarks.length" class="empty">暂无书签</div>
      <div
        v-else
        v-for="(bm, idx) in chapterBookmarks"
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
        <div v-if="bm.content && editingNoteKey !== getBookmarkKey(bm)" class="bm-note">
          {{ bm.content }}
        </div>
        <div v-if="editingNoteKey === getBookmarkKey(bm)" class="bm-note-editor" @click.stop>
          <textarea
            v-model="editingNoteText"
            rows="2"
            placeholder="写点备注..."
            @keydown.esc.stop="cancelNoteEdit"
          ></textarea>
          <div class="bm-note-actions">
            <button class="note-btn primary" @click.stop="saveNoteEdit(bm)">保存</button>
            <button class="note-btn" @click.stop="cancelNoteEdit">取消</button>
          </div>
        </div>
        <div v-if="!bookmarkEditMode" class="bm-ops">
          <button
            class="bm-edit"
            :title="bm.content ? '编辑备注' : '添加备注'"
            @click.stop="startNoteEdit(bm)"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M12 20h9" />
              <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z" />
            </svg>
          </button>
          <button class="bm-delete" @click.stop="handleDeleteBookmark(bm)">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path
                d="M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"
              />
            </svg>
          </button>
        </div>
      </div>
    </div>

    <!-- Underlines List -->
    <div v-show="activeTab === 'underlines'" class="list-container">
      <div v-if="!underlineBookmarks.length" class="empty">暂无划线</div>
      <div
        v-else
        v-for="(bm, idx) in underlineBookmarks"
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
        <div class="bm-snippet underline-snippet">{{ bm.bookText }}</div>
        <div v-if="bm.content && editingNoteKey !== getBookmarkKey(bm)" class="bm-note">
          {{ bm.content }}
        </div>
        <div v-if="editingNoteKey === getBookmarkKey(bm)" class="bm-note-editor" @click.stop>
          <textarea
            v-model="editingNoteText"
            rows="2"
            placeholder="写点备注..."
            @keydown.esc.stop="cancelNoteEdit"
          ></textarea>
          <div class="bm-note-actions">
            <button class="note-btn primary" @click.stop="saveNoteEdit(bm)">保存</button>
            <button class="note-btn" @click.stop="cancelNoteEdit">取消</button>
          </div>
        </div>
        <div v-if="!bookmarkEditMode" class="bm-ops">
          <button
            class="bm-edit"
            :title="bm.content ? '编辑备注' : '添加备注'"
            @click.stop="startNoteEdit(bm)"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M12 20h9" />
              <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z" />
            </svg>
          </button>
          <button class="bm-delete" @click.stop="handleDeleteBookmark(bm)">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path
                d="M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"
              />
            </svg>
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, nextTick, watch } from 'vue'
import { useReaderStore } from '../../stores/reader'
import { useReaderContentStore } from '../../stores/readerContent'
import { useAppStore } from '../../stores/app'
import type { Bookmark } from '../../types'
import { listBrowserCachedChapterUrls } from '../../utils/browserCache'
import { isLocalBook } from '../../utils/localBook'
import { bookmarkKindOf, splitBookmarksByKind } from '../../utils/bookmarks'
import { formatMonthDayTime, formatChapterTag, formatChapterWordCount } from '../../utils/format'
import type { BookChapter } from '../../types'

const props = withDefaults(
  defineProps<{
    initialTab?: 'chapters' | 'bookmarks' | 'underlines'
  }>(),
  {
    initialTab: 'chapters',
  }
)
const emit = defineEmits<{
  jumpChapter: [index: number]
  jumpBookmark: [bookmark: Bookmark]
}>()

type CatalogTab = 'chapters' | 'bookmarks' | 'underlines'

const store = useReaderStore()
const contentStore = useReaderContentStore()
const appStore = useAppStore()
const theme = computed(() => store.currentTheme)
const activeTab = ref<CatalogTab>(props.initialTab)
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

// 卷分组：书源目录规则的 volumeName 挂在每个章节上，卷名变化处插入卷标题行；
// meta 行（字数/时间）在此预算好，避免模板里对千章列表重复跑格式化
type CatalogRow =
  | { kind: 'volume'; key: string; name: string }
  | { kind: 'chapter'; key: string; chapter: BookChapter; meta: string }

const catalogRows = computed<CatalogRow[]>(() => {
  const rows: CatalogRow[] = []
  let lastVolume: string | null = null
  for (const chapter of filteredChapters.value) {
    const volume = chapter.volume?.trim() || null
    if (volume && volume !== lastVolume) {
      rows.push({ kind: 'volume', key: `vol-${chapter.index}`, name: volume })
    }
    lastVolume = volume
    const meta = [formatChapterWordCount(chapter.wordCount), formatChapterTag(chapter.tag)]
      .filter(Boolean)
      .join(' · ')
    rows.push({ kind: 'chapter', key: `ch-${chapter.index}`, chapter, meta })
  }
  return rows
})

onMounted(() => {
  activeTab.value = props.initialTab
  scrollToCurrent()
  store.fetchBookmarks()
  void refreshCachedChapterState()
})

/** 旧数据没有 kind 字段，按章书签归组 */
const bookmarkKind = bookmarkKindOf

const chapterBookmarks = computed(() => splitBookmarksByKind(store.bookmarks).bookmarks)
const underlineBookmarks = computed(() => splitBookmarksByKind(store.bookmarks).underlines)

function switchTab(tab: CatalogTab) {
  if (activeTab.value === tab) return
  activeTab.value = tab
  // 批量编辑状态不跨 tab 携带，避免在另一个列表里看不见选中的项
  bookmarkEditMode.value = false
  selectedBookmarkKeys.value.clear()
}

watch(
  () => props.initialTab,
  (tab) => {
    activeTab.value = tab
    if (tab === 'chapters') {
      bookmarkEditMode.value = false
      selectedBookmarkKeys.value.clear()
      void refreshCachedChapterState()
    }
  }
)

watch(
  () => store.book?.bookId,
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
  await contentStore.refreshChapters()
  await refreshCachedChapterState()
  scrollToCurrent()
}

async function refreshCachedChapterState() {
  if (!store.book?.bookId || isLocalBook(store.book) || !store.chapters.length) {
    cachedChapterUrls.value = new Set()
    return
  }
  cachedChapterUrls.value = await listBrowserCachedChapterUrls(store.book.bookId).catch(
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
  return `${bm.bookName}|${bm.bookAuthor}|${bm.chapterIndex}|${bm.chapterPos}|${bookmarkKind(bm)}|${bm.time}|${bm.bookText}`
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

/** 单条删除补二次确认（与批量删除、删书、删书源同一惯例） */
async function handleDeleteBookmark(bm: Bookmark) {
  const label = bookmarkKind(bm) === 'underline' ? '划线' : '书签'
  if (!confirm(`确定删除这条${label}吗？`)) return
  try {
    await store.removeBookmark(bm)
    appStore.showToast(`已删除${label}`, 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || `删除${label}失败`, 'error')
  }
}

/* ─── 备注编辑（写 Bookmark.content 列） ─── */
const editingNoteKey = ref('')
const editingNoteText = ref('')

function startNoteEdit(bm: Bookmark) {
  editingNoteKey.value = getBookmarkKey(bm)
  editingNoteText.value = bm.content || ''
}

function cancelNoteEdit() {
  editingNoteKey.value = ''
  editingNoteText.value = ''
}

async function saveNoteEdit(bm: Bookmark) {
  try {
    await store.updateBookmarkNote(bm, editingNoteText.value.trim())
    cancelNoteEdit()
    appStore.showToast('备注已保存', 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || '备注保存失败', 'error')
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
  // 只取当前 tab 类型的条目：选集带 kind 不撞键，但批量删除不跨类型
  const source = activeTab.value === 'underlines' ? underlineBookmarks.value : chapterBookmarks.value
  const items = source.filter((bookmark) =>
    selectedBookmarkKeys.value.has(getBookmarkKey(bookmark))
  )
  if (!items.length) {
    bookmarkEditMode.value = false
    selectedBookmarkKeys.value.clear()
    return
  }
  const label = activeTab.value === 'underlines' ? '划线' : '书签'
  if (!confirm(`确定删除选中的 ${items.length} 条${label}吗？`)) return
  try {
    await store.removeBookmarks(items)
    selectedBookmarkKeys.value.clear()
    bookmarkEditMode.value = false
    appStore.showToast(`已删除 ${items.length} 条${label}`, 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || `删除${label}失败`, 'error')
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

.icon-btn:hover:not(:disabled),
.close-btn:hover {
  opacity: 1;
  background: rgba(0, 0, 0, 0.05);
}

.icon-btn:disabled {
  opacity: 0.35;
  cursor: default;
}

/* 图标限定 18px：不加约束会撑满 32px 按钮，比全站其他关闭/刷新键大一圈 */
.icon-btn svg,
.close-btn svg {
  width: 18px;
  height: 18px;
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

/* 带 meta 行（字数/时间）的章节更高，给 content-visibility 一个更准的预估 */
.list-item.with-meta {
  contain-intrinsic-size: auto 64px;
}

.volume-header {
  padding: 10px 20px 6px;
  font-size: var(--text-sm);
  font-weight: 600;
  opacity: 0.65;
  line-height: 1.4;
  cursor: default;
  content-visibility: auto;
  contain-intrinsic-size: auto 34px;
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

.item-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.item-title {
  font-size: var(--text-base);
  line-height: 1.4;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.item-meta {
  font-size: var(--text-xs);
  opacity: 0.45;
  line-height: 1.3;
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
  /* .list-item 的 align-items:center 在纵向布局下会把内容块水平居中，
     书签/划线总览要统一左对齐（内容块拉满整宽，文本自然靠左） */
  align-items: stretch;
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
  /* 书签/划线总览统一左对齐：章节名与时间同行依次排 */
  justify-content: flex-start;
  align-items: baseline;
  gap: 10px;
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

/* 划线列表的主角是摘录文本本身，提到主字号 */
.underline-snippet {
  font-size: var(--text-sm);
  opacity: 0.85;
  -webkit-line-clamp: 3;
}

.bm-ops {
  position: absolute;
  right: 12px;
  bottom: 12px;
  display: flex;
  gap: 6px;
  opacity: 0;
  transition: 0.2s;
}

.bookmark-item:hover .bm-ops,
.bm-ops:focus-within {
  opacity: 1;
}

.bm-edit,
.bm-delete {
  width: 28px;
  height: 28px;
  display: flex;
  align-items: center;
  justify-content: center;
  border: none;
  border-radius: 4px;
  cursor: pointer;
  transition: 0.2s;
}

.bm-edit {
  background: rgba(0, 0, 0, 0.05);
  color: inherit;
}

.bm-delete {
  background: rgba(255, 0, 0, 0.05);
  color: #ef4444;
}

.bm-edit svg,
.bm-delete svg {
  width: 14px;
  height: 14px;
}

.bm-note {
  font-size: var(--text-xs);
  opacity: 0.75;
  background: rgba(201, 127, 58, 0.08);
  border-left: 2px solid var(--color-primary, #c97f3a);
  padding: 4px 8px;
  border-radius: 4px;
  line-height: 1.5;
  white-space: pre-wrap;
  word-break: break-word;
}

.bm-note-editor textarea {
  width: 100%;
  box-sizing: border-box;
  background: rgba(0, 0, 0, 0.04);
  border: 1px solid rgba(0, 0, 0, 0.1);
  border-radius: 8px;
  padding: 6px 10px;
  color: inherit;
  font-size: var(--text-xs);
  font-family: inherit;
  outline: none;
  resize: vertical;
}

.bm-note-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 6px;
}

.note-btn {
  font-size: var(--text-xs);
  padding: 3px 12px;
  border-radius: 6px;
  border: 1px solid var(--color-border, rgba(0, 0, 0, 0.12));
  background: transparent;
  color: inherit;
  cursor: pointer;
}

.note-btn.primary {
  background: var(--color-primary, #c97f3a);
  border-color: var(--color-primary, #c97f3a);
  color: #fff;
}
</style>
