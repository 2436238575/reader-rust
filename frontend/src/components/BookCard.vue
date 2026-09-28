<template>
  <div class="book-card" :class="{ 'edit-mode': editMode, 'selected': selected, 'dragging': dragging, 'search-mode': isSearch }"
    @click="handleCardClick">
    <!-- Cover -->
    <div class="card-cover" @click.stop="handleCoverClick">
      <button v-if="showDeleteAction && !editMode" class="card-delete-btn" title="删除最近阅读"
        @click.stop="$emit('delete', book)">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M18 6 6 18M6 6l12 12" />
        </svg>
      </button>
      <img v-if="coverSrc" :src="coverSrc" :alt="book.name" class="cover-img" loading="lazy"
        @error="coverFailed = true" />
      <div v-else class="cover-placeholder">
        <span class="cover-title">{{ book.name }}</span>
        <span class="cover-author">{{ book.author }}</span>
      </div>

      <!-- Unread badge -->
      <div v-if="unreadCount > 0 && !editMode" class="unread-badge">
        {{ unreadCount > 99 ? '99+' : unreadCount }}
      </div>

      <!-- Selection overlay -->
      <div v-if="editMode" class="selection-overlay">
        <div class="checkbox" :class="{ checked: selected }">
          <svg v-if="selected" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3">
            <path d="M20 6 9 17l-5-5" />
          </svg>
        </div>
      </div>
    </div>

    <!-- Info -->
    <div class="card-info">
      <div class="card-topline">
        <div class="title-block">
          <h3 class="book-name">{{ book.name }}</h3>
          <div class="book-meta">
            <span class="book-author">{{ book.author || '未知作者' }}</span>
            <span v-if="asBook.totalChapterNum" class="meta-dot">·</span>
            <span v-if="asBook.totalChapterNum" class="book-chapters">共{{ asBook.totalChapterNum }}章</span>
          </div>
        </div>
      </div>

      <div v-if="isSearch && introText" class="book-intro-row">
        <p class="book-intro" :class="{ 'intro-3-lines': isSearch }">{{ introText }}</p>
      </div>

      <div v-if="(!isSearch && asBook.durChapterTitle) || latestChapterText" class="chapter-lines">
        <p v-if="asBook.durChapterTitle && !isSearch" class="book-progress">
          已读：{{ asBook.durChapterTitle }}
        </p>
        <p v-if="latestChapterText" class="book-latest">
          最新：{{ latestChapterText }}
        </p>
      </div>

      <div class="card-footer" :class="{ 'search-footer': isSearch }">
        <div v-if="!isSearch && (browserCachedCount > 0 || serverCachedCount > 0)" class="book-cache-row">
          <span v-if="browserCachedCount > 0" class="cache-chip primary">本地 {{ browserCachedCount }} 章</span>
          <span v-if="serverCachedCount > 0" class="cache-chip">远程 {{ serverCachedCount }} 章</span>
        </div>
        <!-- Search mode: add to shelf -->
        <button v-if="isSearch && onShelf" class="add-shelf-btn on-shelf" disabled title="已加入书架">
          已在书架
        </button>
        <button v-else-if="isSearch" class="add-shelf-btn" :disabled="adding" @click.stop="$emit('addToShelf', book)">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="14" height="14">
            <path d="M12 5v14M5 12h14" />
          </svg>
          {{ adding ? '加入中...' : '加入书架' }}
        </button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { getCoverUrl } from '../api/bookshelf'
import { isLocalBook } from '../utils/localBook'
import type { Book, SearchBook } from '../types'

const props = defineProps<{
  book: Book | SearchBook
  editMode?: boolean
  selected?: boolean
  isSearch?: boolean
  dragging?: boolean
  showDeleteAction?: boolean
  /** 搜索/发现卡片：该书已在书架中 */
  onShelf?: boolean
  /** 搜索/发现卡片：加入书架请求进行中（防连点） */
  adding?: boolean
}>()

const emit = defineEmits<{
  click: [book: Book | SearchBook]
  info: [book: Book | SearchBook]
  delete: [book: Book | SearchBook]
  addToShelf: [book: Book | SearchBook]
  select: [book: Book | SearchBook]
}>()

function handleCardClick() {
  if (props.editMode) {
    emit('select', props.book)
  } else {
    emit('click', props.book)
  }
}

function handleCoverClick() {
  if (props.editMode) {
    emit('select', props.book)
  } else {
    emit('info', props.book)
  }
}

const coverFailed = ref(false)

const asBook = computed(() => props.book as Book)
const asSearchBook = computed(() => props.book as SearchBook)

const coverSrc = computed(() => {
  if (coverFailed.value) return ''
  const url = (props.book as Book).customCoverUrl || props.book.coverUrl
  if (!url) return ''
  return getCoverUrl(url)
})

const unreadCount = computed(() => {
  const b = props.book as Book
  if (!b.totalChapterNum || b.durChapterIndex === undefined) return 0
  return Math.max(0, b.totalChapterNum - 1 - b.durChapterIndex)
})

const browserCachedCount = computed(() => isLocalBook(asBook.value) ? 0 : Math.max(0, asBook.value.browserCachedChapterCount || 0))
const serverCachedCount = computed(() => isLocalBook(asBook.value) ? 0 : Math.max(0, asBook.value.cachedChapterCount || 0))
const latestChapterText = computed(() => {
  if (props.isSearch) {
    return asSearchBook.value.lastChapter || asBook.value.latestChapterTitle || ''
  }
  return asBook.value.latestChapterTitle || ''
})
const introText = computed(() => {
  if (!props.isSearch) return ''
  return asSearchBook.value.intro?.trim() || ''
})
</script>

<style scoped>
.book-card {
  display: grid;
  grid-template-columns: 86px minmax(0, 1fr);
  align-items: center;
  gap: 15px;
  height: 100%;
  min-height: 148px;
  padding: 13px 14px;
  border-radius: 8px;
  border: 1px solid var(--color-border-light);
  background: var(--color-bg-elevated);
  cursor: pointer;
  transition:
    background var(--duration-normal) var(--ease-out),
    border-color var(--duration-normal) var(--ease-out),
    box-shadow var(--duration-normal) var(--ease-out),
    transform var(--duration-normal) var(--ease-out);
  position: relative;
  overflow: hidden;
}

.book-card::before {
  content: "";
  position: absolute;
  inset: 0;
  background:
    radial-gradient(circle at 0 0, rgba(201, 127, 58, 0.08), transparent 34%),
    linear-gradient(90deg, rgba(201, 127, 58, 0.035), transparent 42%);
  opacity: 0;
  transition: opacity var(--duration-normal) var(--ease-out);
  pointer-events: none;
}

.book-card:hover {
  border-color: var(--color-primary-border);
  box-shadow:
    0 12px 26px rgba(39, 32, 22, 0.09),
    0 1px 7px rgba(39, 32, 22, 0.04);
  transform: translateY(-1px);
}

.book-card:hover::before {
  opacity: 1;
}

.book-card:active {
  transform: translateY(0);
}

.card-cover {
  width: 86px;
  height: 118px;
  flex-shrink: 0;
  border-radius: 6px;
  overflow: hidden;
  position: relative;
  background: var(--color-bg-sunken);
  box-shadow:
    0 10px 18px rgba(48, 35, 20, 0.16),
    0 0 0 1px rgba(0, 0, 0, 0.04);
}

.card-cover::after {
  content: "";
  position: absolute;
  inset: 0 auto 0 0;
  width: 13px;
  background: linear-gradient(90deg, rgba(0, 0, 0, 0.16), transparent);
  pointer-events: none;
}

.cover-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.cover-placeholder {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: var(--space-2);
  background:
    linear-gradient(160deg, rgba(212, 129, 42, 0.12), rgba(70, 134, 121, 0.08)),
    var(--color-bg-sunken);
  text-align: center;
  gap: var(--space-1);
}

.cover-title {
  font-size: var(--text-xs);
  font-weight: 600;
  color: var(--color-primary);
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  line-height: var(--leading-tight);
}

.cover-author {
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.unread-badge {
  position: absolute;
  top: 8px;
  right: 8px;
  background: linear-gradient(135deg, var(--color-primary), var(--color-primary-dark));
  color: white;
  font-size: var(--text-xs);
  font-weight: 700;
  padding: 2px 7px;
  border-radius: var(--radius-full);
  min-width: 18px;
  text-align: center;
  line-height: 15px;
  box-shadow: 0 6px 14px rgba(201, 127, 58, 0.28);
  z-index: 1;
}

.card-delete-btn {
  position: absolute;
  top: var(--space-1);
  left: var(--space-1);
  z-index: 2;
  width: 28px;
  height: 28px;
  border-radius: 999px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(12, 14, 18, 0.58);
  color: white;
  backdrop-filter: blur(8px);
  transition: transform var(--duration-fast), background var(--duration-fast);
}

.card-delete-btn:hover {
  background: rgba(185, 44, 44, 0.9);
  transform: scale(1.06);
}

.card-delete-btn svg {
  width: 14px;
  height: 14px;
}

.selection-overlay {
  position: absolute;
  inset: 0;
  background: rgba(var(--color-primary-rgb), 0.1);
  display: flex;
  align-items: flex-start;
  justify-content: flex-end;
  padding: var(--space-1);
  opacity: 1;
}

.checkbox {
  width: 24px;
  height: 24px;
  border-radius: var(--radius-full);
  border: 2px solid white;
  background: rgba(0, 0, 0, 0.2);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all var(--duration-fast);
}

.checkbox.checked {
  background: var(--color-primary);
  border-color: var(--color-primary);
}

.checkbox svg {
  width: 14px;
  height: 14px;
  color: white;
}

.book-card.selected {
  border-color: var(--color-primary);
  background: rgba(var(--color-primary-rgb), 0.05);
}

.book-card.dragging {
  opacity: 0.55;
  transform: scale(0.98);
  box-shadow: none;
}

.card-info {
  min-width: 0;
  display: flex;
  flex-direction: column;
  justify-content: center;
  gap: 8px;
  align-self: stretch;
  padding: 2px 0;
  position: relative;
  z-index: 1;
}

.card-topline {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  min-width: 0;
}

.title-block {
  min-width: 0;
  flex: 1;
}

.card-footer {
  display: flex;
  align-items: center;
  justify-content: flex-start;
  flex-wrap: wrap;
  gap: 10px;
  min-width: 0;
  min-height: 25px;
}

.book-name {
  margin: 0;
  font-size: var(--text-lg);
  font-weight: 700;
  color: var(--color-text);
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  line-height: var(--leading-tight);
}

.book-meta {
  display: flex;
  align-items: center;
  gap: var(--space-1);
  min-height: 18px;
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
  white-space: nowrap;
  overflow: hidden;
  margin-top: 4px;
}

.book-author {
  font-size: var(--text-xs);
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}

.meta-dot {
  font-size: var(--text-xs);
}

.book-progress,
.book-latest {
  margin: 0;
  font-size: var(--text-xs);
  color: var(--color-text-secondary);
  display: -webkit-box;
  -webkit-line-clamp: 1;
  -webkit-box-orient: vertical;
  overflow: hidden;
  line-height: var(--leading-normal);
}

.book-latest {
  color: var(--color-text-tertiary);
}

.chapter-lines {
  display: grid;
  gap: 3px;
  min-height: 35px;
}

.book-intro-row {
  min-width: 0;
}

.book-intro {
  margin: 0;
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
  line-height: var(--leading-normal);
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

/* 搜索/发现卡片没有已读/最新行，简介多放一行 */
.book-intro.intro-3-lines {
  -webkit-line-clamp: 2;
}

.book-cache-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  min-width: 0;
  flex: 1 1 auto;
}

.cache-chip {
  display: inline-flex;
  align-items: center;
  min-height: 25px;
  padding: 0 10px;
  border-radius: 999px;
  background: rgba(0, 0, 0, 0.04);
  color: var(--color-text-secondary);
  font-size: var(--text-xs);
  line-height: 1;
  white-space: nowrap;
}

.cache-chip.primary {
  background: rgba(201, 127, 58, 0.13);
  color: var(--color-primary);
}

.card-footer.search-footer {
  justify-content: flex-end;
}

.add-shelf-btn {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  flex: 0 0 auto;
  min-height: 30px;
  padding: 0 12px;
  background: var(--color-primary);
  color: white;
  border-radius: 999px;
  font-size: var(--text-xs);
  font-weight: 700;
  transition: all var(--duration-fast);
}

.add-shelf-btn:hover {
  background: var(--color-primary-dark);
  transform: scale(1.02);
}

.add-shelf-btn:active {
  transform: scale(0.98);
}

.add-shelf-btn.on-shelf {
  background: transparent;
  color: var(--color-text-tertiary);
  border: 1px solid var(--color-border);
  cursor: default;
}

@media (max-width: 520px) {
  .book-card {
    grid-template-columns: 76px minmax(0, 1fr);
    gap: 13px;
    padding: 12px;
  }

  .card-cover {
    width: 76px;
    height: 106px;
  }

  .book-name {
    font-size: var(--text-base);
  }

  .card-footer {
    gap: 7px;
  }
}
</style>
