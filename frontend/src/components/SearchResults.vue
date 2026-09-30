<template>
  <div class="search-results" ref="searchScrollRef">
    <div class="search-header">
      <h2>
        搜索 "{{ searchKey }}"
        <span v-if="isSearching" class="searching-indicator">
          <span class="dot-pulse"></span>
          搜索中...
        </span>
        <span v-else class="result-count">({{ displayResults.length }} 个结果)</span>
      </h2>
      <button class="back-btn" @click="$emit('back')">
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          width="18"
          height="18"
        >
          <path d="M19 12H5" />
          <path d="m12 19-7-7 7-7" />
        </svg>
        返回
      </button>
    </div>

    <!-- 启用的书源多于一个才有范围选择的意义；未加载时默认隐藏 -->
    <div v-if="sourceOptions.length > 1" class="search-filters">
      <div class="filter-tabs" role="tablist" aria-label="搜索范围">
        <button
          type="button"
          class="filter-tab"
          :class="{ active: searchScope === 'all' }"
          @click="searchScope = 'all'"
        >
          全部书源
        </button>
        <button
          type="button"
          class="filter-tab"
          :class="{ active: searchScope === 'group' }"
          @click="searchScope = 'group'"
        >
          按分组
        </button>
        <button
          type="button"
          class="filter-tab"
          :class="{ active: searchScope === 'source' }"
          @click="searchScope = 'source'"
        >
          单个书源
        </button>
      </div>

      <div v-if="searchScope === 'group'" class="filter-select-wrap">
        <select v-model="selectedGroup" class="filter-select">
          <option v-for="group in sourceGroups" :key="group" :value="group">
            {{ group }}
          </option>
        </select>
      </div>

      <div v-else-if="searchScope === 'source'" class="filter-select-wrap">
        <select v-model="selectedSourceUrl" class="filter-select">
          <option
            v-for="source in sourceOptions"
            :key="source.bookSourceUrl"
            :value="source.bookSourceUrl"
          >
            {{ source.bookSourceName }}
          </option>
        </select>
      </div>
    </div>

    <!-- 搜索失败：不能与「未找到」混为一谈 -->
    <div v-if="searchFailed && !displayResults.length && !isSearching" class="search-failed">
      <p>搜索失败或连接中断</p>
      <button class="search-retry-btn" @click="retrySearch">重试</button>
    </div>

    <BookGrid
      v-else
      :books="displayResults"
      :is-search="true"
      :loading="isSearching && displayResults.length === 0"
      :shelf-urls="shelfUrls"
      :adding-urls="addingUrls"
      empty-text="未找到相关书籍"
      @click="handleBookClick"
      @info="handleBookInfo"
      @addToShelf="handleAddToShelf"
    />

    <BookDetailModal v-model="showBookDetail" :book="selectedBook" />
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { usePreserveScroll } from '../composables/usePreserveScroll'
import { useBookshelfStore } from '../stores/bookshelf'
import { useAppStore } from '../stores/app'
import { useSourceStore } from '../stores/source'
import { searchBookMultiSSE } from '../api/search'
import { saveBook } from '../api/bookshelf'
import { useOpenBook } from '../composables/useOpenBook'
import { splitBookSourceGroups } from '../utils/sourceSelection'
import BookGrid from './BookGrid.vue'
import BookDetailModal from './BookDetailModal.vue'
import type { Book, SearchBook } from '../types'

import { storeToRefs } from 'pinia'

const shelfStore = useBookshelfStore()
const appStore = useAppStore()
const sourceStore = useSourceStore()
const { openBook } = useOpenBook()

const {
  searchKey,
  searchResults: results,
  isSearching,
  searchScope,
  searchGroup: selectedGroup,
  searchSourceUrl: selectedSourceUrl,
} = storeToRefs(shelfStore)

let eventSource: EventSource | null = null
// 跨 SSE 消息维护的去重集合；每次发起新搜索时重建
let searchSeen: Set<string> | null = null
const showBookDetail = ref(false)
const selectedBook = ref<Book | SearchBook | null>(null)

// 已入库书籍把按钮切为「已在书架」（此前漏传，可重复添加同一本书）
const shelfUrls = computed(() => new Set(shelfStore.books.map((book) => book.bookUrl)))
// 加入书架请求进行中：按 bookUrl 防连点
const addingUrls = ref(new Set<string>())
// SSE 连接失败/中断：此前只置 isSearching=false，用户看到误导性的「未找到相关书籍」
const searchFailed = ref(false)

// 保活恢复时 Chromium 已丢失内部滚动位置，手动回填
const searchScrollRef = ref<HTMLElement>()
usePreserveScroll(() => searchScrollRef.value)

const sourceByUrl = computed(() => {
  return new Map(sourceStore.sources.map((source) => [source.bookSourceUrl, source]))
})

const sourceGroups = computed(() => {
  const groups = new Set<string>()
  for (const source of sourceStore.sources) {
    for (const group of splitBookSourceGroups(source.bookSourceGroup)) {
      groups.add(group)
    }
  }
  return Array.from(groups).sort((a, b) => a.localeCompare(b, 'zh-Hans-CN'))
})

const sourceOptions = computed(() => {
  return [...sourceStore.sources]
    .filter((source) => source.enabled !== false)
    .sort((a, b) => {
      const orderDiff = (a.customOrder ?? 0) - (b.customOrder ?? 0)
      if (orderDiff !== 0) return orderDiff
      return a.bookSourceName.localeCompare(b.bookSourceName, 'zh-Hans-CN')
    })
})

const displayResults = computed<SearchBook[]>(() => {
  // SSE 插入时已补齐源信息（见 onmessage），这里直接复用原对象，
  // 保证同一结果跨批次对象身份稳定，BookGrid 的 keyed diff 才能跳过
  return results.value.map((book) => {
    if (book.originName) return book
    const source = sourceByUrl.value.get(book.origin)
    return {
      ...book,
      originName: book.originName || source?.bookSourceName || book.origin,
      originGroup: book.originGroup || source?.bookSourceGroup,
    }
  })
})

function closeEventSource() {
  if (eventSource) {
    eventSource.close()
    eventSource = null
  }
}

function ensureSearchSelection() {
  if (searchScope.value === 'group') {
    const selectedGroupStillValid =
      selectedGroup.value && sourceGroups.value.includes(selectedGroup.value)
    if (!selectedGroupStillValid && sourceGroups.value.length > 0) {
      selectedGroup.value = sourceGroups.value[0]
    }
  }
  if (searchScope.value === 'source') {
    const selectedSourceStillValid = sourceOptions.value.some(
      (source) => source.bookSourceUrl === selectedSourceUrl.value
    )
    if (!selectedSourceStillValid && sourceOptions.value.length > 0) {
      selectedSourceUrl.value = sourceOptions.value[0].bookSourceUrl
    }
  }
}

function doSearch(key: string) {
  closeEventSource()
  searchSeen = new Set()
  searchFailed.value = false

  // 只启用一个书源时筛选区整条隐藏，范围等同「全部书源」——
  // 避免上次选过的分组/单源在隐藏后仍悄悄限制结果
  const effectiveScope = sourceOptions.value.length === 1 ? 'all' : searchScope.value

  if (effectiveScope === 'group' && !selectedGroup.value) {
    shelfStore.searchResults = []
    shelfStore.isSearching = false
    return
  }

  if (effectiveScope === 'source' && !selectedSourceUrl.value) {
    shelfStore.searchResults = []
    shelfStore.isSearching = false
    return
  }

  shelfStore.searchResults = []
  shelfStore.isSearching = true

  eventSource = searchBookMultiSSE({
    key,
    concurrentCount: 24,
    bookSourceGroup: effectiveScope === 'group' ? selectedGroup.value : undefined,
    bookSourceUrl: effectiveScope === 'source' ? selectedSourceUrl.value : undefined,
  })

  eventSource.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data)
      if (data.data && Array.isArray(data.data)) {
        // 去重集合跨消息维护：此前每条 SSE 消息都重建一次全量 Set（O(已到条数)）
        if (!searchSeen) searchSeen = new Set()
        const annotate = (b: SearchBook) => {
          const source = sourceByUrl.value.get(b.origin)
          return {
            ...b,
            originName: b.originName || source?.bookSourceName || b.origin,
            originGroup: b.originGroup || source?.bookSourceGroup,
          }
        }
        const newBooks: SearchBook[] = []
        for (const b of data.data as SearchBook[]) {
          const dedupeKey = `${b.origin}::${b.bookUrl}`
          if (searchSeen.has(dedupeKey)) continue
          searchSeen.add(dedupeKey)
          newBooks.push(annotate(b))
        }
        if (newBooks.length) {
          shelfStore.searchResults = shelfStore.searchResults.concat(newBooks)
        }
      }
    } catch {
      /* skip */
    }
  }

  eventSource.addEventListener('end', () => {
    shelfStore.isSearching = false
    closeEventSource()
  })

  eventSource.addEventListener('error', () => {
    shelfStore.isSearching = false
    searchFailed.value = true
    closeEventSource()
  })

  eventSource.onerror = () => {
    shelfStore.isSearching = false
    searchFailed.value = true
    closeEventSource()
  }
}

function retrySearch() {
  if (!searchKey.value || shelfStore.isSearching) return
  doSearch(searchKey.value)
}

watch(
  [() => shelfStore.searchKey, searchScope, selectedGroup, selectedSourceUrl],
  ([key]) => {
    ensureSearchSelection()
    if (key) {
      doSearch(key)
    } else {
      closeEventSource()
      shelfStore.searchResults = []
      shelfStore.isSearching = false
    }
  },
  { immediate: true }
)

watch(
  [searchScope, sourceGroups, sourceOptions],
  () => {
    ensureSearchSelection()
  },
  { immediate: true }
)

onMounted(async () => {
  if (sourceStore.sources.length === 0) {
    await sourceStore.fetchSources().catch(() => undefined)
  }
  ensureSearchSelection()
})

onUnmounted(() => {
  closeEventSource()
})

async function handleBookClick(book: Book | SearchBook) {
  await openBook(book)
}

function handleBookInfo(book: Book | SearchBook) {
  selectedBook.value = book
  showBookDetail.value = true
}

async function handleAddToShelf(book: Book | SearchBook) {
  if (addingUrls.value.has(book.bookUrl)) return
  addingUrls.value.add(book.bookUrl)
  try {
    await saveBook({
      name: book.name,
      author: book.author,
      bookUrl: book.bookUrl,
      origin: book.origin,
      coverUrl: book.coverUrl,
    })
    appStore.showToast(`"${book.name}" 已加入书架`, 'success')
    // 刷新书架列表让按钮翻转为「已在书架」；失败不影响主流程
    await shelfStore.fetchBooks().catch(() => undefined)
  } catch (e: unknown) {
    appStore.showToast((e as Error).message, 'error')
  } finally {
    addingUrls.value.delete(book.bookUrl)
  }
}

defineEmits<{
  back: []
}>()
</script>

<style scoped>
.search-results {
  height: 100%;
  min-height: 0;
  overflow: auto;
  padding: 0 var(--space-6);
}

.search-failed {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-3);
  padding: 64px 0;
  color: var(--color-danger);
  font-size: var(--text-base);
}

.search-retry-btn {
  padding: 6px 24px;
  border-radius: var(--radius-md);
  border: 1px solid currentColor;
  background: transparent;
  color: inherit;
  font-size: var(--text-sm);
  cursor: pointer;
}

.search-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--space-4) 0;
  gap: var(--space-4);
}

.search-header h2 {
  font-size: var(--text-xl);
  font-weight: 700;
  display: flex;
  align-items: center;
  gap: var(--space-3);
}

.result-count {
  font-size: var(--text-sm);
  font-weight: 400;
  color: var(--color-text-tertiary);
}

.searching-indicator {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  font-size: var(--text-sm);
  font-weight: 400;
  color: var(--color-primary);
}

.dot-pulse {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--color-primary);
  animation: pulse 1.2s infinite ease-in-out;
}

@keyframes pulse {
  0%,
  80%,
  100% {
    transform: scale(0.6);
    opacity: 0.5;
  }
  40% {
    transform: scale(1);
    opacity: 1;
  }
}

.back-btn {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-2) var(--space-4);
  border-radius: var(--radius-md);
  font-size: var(--text-sm);
  font-weight: 500;
  color: var(--color-text-secondary);
  border: 1px solid var(--color-border);
  transition: all var(--duration-fast);
}

.back-btn:hover {
  background: var(--color-bg-hover);
  color: var(--color-text);
}

.search-filters {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-3);
  margin-bottom: var(--space-5);
}

.filter-tabs {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  padding: 4px;
  border-radius: var(--radius-full);
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-border-light);
}

.filter-tab {
  min-height: 34px;
  padding: 0 var(--space-4);
  border-radius: var(--radius-full);
  font-size: var(--text-sm);
  font-weight: 500;
  color: var(--color-text-secondary);
  transition: all var(--duration-fast);
}

.filter-tab:hover {
  color: var(--color-text);
  background: var(--color-bg-hover);
}

.filter-tab.active {
  color: white;
  background: var(--color-primary);
}

.filter-select-wrap {
  min-width: min(100%, 280px);
}

.filter-select {
  width: 100%;
  min-height: 40px;
  padding: 0 var(--space-4);
  border-radius: var(--radius-lg);
  border: 1px solid var(--color-border);
  background: var(--color-bg-elevated);
  color: var(--color-text);
  font-size: var(--text-sm);
}

@media (max-width: 720px) {
  .search-results {
    padding: 0 var(--space-4);
  }

  .search-header {
    flex-direction: column;
    align-items: stretch;
  }

  .search-header h2 {
    flex-wrap: wrap;
  }

  .back-btn {
    justify-content: center;
  }

  .filter-tabs {
    width: 100%;
    justify-content: space-between;
  }

  .filter-tab {
    flex: 1;
    padding: 0 var(--space-2);
  }

  .filter-select-wrap {
    width: 100%;
    min-width: 0;
  }
}
</style>
