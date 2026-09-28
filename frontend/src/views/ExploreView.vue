<template>
  <div class="explore-view" :style="{ '--color-primary': '#c97f3a' }">
    <div class="explore-header">
      <h2>发现</h2>
      <!-- 顶部：书源切换（与标题同行，右对齐） -->
      <div class="source-selector">
        <select :value="store.activeSourceUrl" @change="onSourceChange" v-if="store.exploreSources.length > 0">
          <option
            v-for="src in store.exploreSources"
            :key="src.bookSourceUrl"
            :value="src.bookSourceUrl"
          >
            {{ src.bookSourceName }}
          </option>
        </select>
        <span v-else class="no-sources-text">无带有发现规则的书源</span>
      </div>
    </div>

    <div class="explore-body">
      <!-- 桌面端侧边分类 / 移动端顶部分类滑动条 -->
      <div class="categories-panel">
        <div class="categories-scroll">
          <div
            v-for="(cat, index) in store.categories"
            :key="getExploreCategoryKey(cat, index)"
            class="category-tag"
            :class="{
              active: !isExploreCategorySection(cat) && store.activeCategoryUrl === cat.url,
              section: isExploreCategorySection(cat),
            }"
            @click="handleCategoryClick(cat)"
          >
            {{ cat.title }}
          </div>
        </div>
      </div>

      <!-- 书籍列表区 -->
      <div class="content-panel" ref="scrollContainer" @scroll="tryFetchMore">
        <div class="books-grid-wrapper" v-if="store.books.length > 0">
          <BookGrid
            :books="store.books"
            :is-search="true"
            :shelf-urls="shelfUrls"
            empty-text="暂无数据"
            @click="handleBookClick"
            @info="handleBookInfo"
            @addToShelf="handleAddToShelf"
          />
        </div>

        <div class="loading-state" v-if="store.loading">
          <div class="spinner"></div>加载中...
        </div>

        <div class="end-state" v-else-if="!store.hasMore && store.books.length > 0">
          没有更多了
        </div>

        <div class="error-state" v-else-if="store.error">
          {{ store.error }}
        </div>

        <!-- 滚动加载哨兵：距底部 200px 内即翻页 -->
        <div ref="sentinelRef" class="load-more-sentinel" aria-hidden="true"></div>
      </div>
    </div>

    <BookDetailModal v-model="showDetail" :book="selectedBook" />
  </div>
</template>

<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { useExploreStore } from '../stores/explore'
import { useBookshelfStore } from '../stores/bookshelf'
import { useOpenBook } from '../composables/useOpenBook'
import { saveBook } from '../api/bookshelf'
import { useAppStore } from '../stores/app'
import BookGrid from '../components/BookGrid.vue'
import BookDetailModal from '../components/BookDetailModal.vue'
import type { Book, SearchBook } from '../types'
import {
  getExploreCategoryKey,
  isExploreCategorySection,
  type ExploreCategory,
} from '../utils/exploreCategories'

const store = useExploreStore()
const shelfStore = useBookshelfStore()
const appStore = useAppStore()
const { openBook } = useOpenBook()

const scrollContainer = ref<HTMLElement>()
const sentinelRef = ref<HTMLElement>()
const showDetail = ref(false)
const selectedBook = ref<Book | SearchBook | null>(null)

// 书架上已有书籍的 bookUrl 集合：发现卡片把「加入书架」切为「已在书架」
const shelfUrls = computed(() => new Set(shelfStore.books.map((book) => book.bookUrl)))

onMounted(async () => {
  await store.init()
  tryFetchMore()
})

onUnmounted(() => {
  stopWatchers()
})

// 哨兵（列表末尾占位）距面板底部 200px 内视为「该翻页」；
// 视口大到最后一批也放得下时 scroll 永不触发，靠加载完成后的 watch 链式补页
function nearBottom() {
  const panel = scrollContainer.value
  const sentinel = sentinelRef.value
  if (!panel || !sentinel) return false
  const panelTop = panel.getBoundingClientRect().top
  const sentinelTop = sentinel.getBoundingClientRect().top
  return sentinelTop - panelTop <= panel.clientHeight + 200
}

function tryFetchMore() {
  if (!store.loading && store.hasMore && nearBottom()) {
    store.fetchMore()
  }
}

const stopWatchers = watch(
  [() => store.loading, () => store.books.length],
  () => tryFetchMore(),
  // 必须等 DOM 渲染完再量哨兵位置，否则拿到的是上一轮布局
  { flush: 'post' },
)

function onSourceChange(event: Event) {
  store.setSource((event.target as HTMLSelectElement).value)
}

function handleCategoryClick(category: ExploreCategory) {
  if (isExploreCategorySection(category)) return
  store.setCategory(category.url)
}

async function handleBookClick(book: Book | SearchBook) {
  await openBook(book)
}

function handleBookInfo(book: Book | SearchBook) {
  selectedBook.value = book
  showDetail.value = true
}

async function handleAddToShelf(book: Book | SearchBook) {
  try {
    await saveBook({
      name: book.name,
      author: book.author,
      bookUrl: book.bookUrl,
      origin: book.origin,
      coverUrl: book.coverUrl,
    })
    appStore.showToast(`"${book.name}" 已加入书架`, 'success')
    // 刷新书架列表，让「加入书架」按钮翻转为「已在书架」
    await shelfStore.fetchBooks().catch(() => undefined)
  } catch (e: unknown) {
    appStore.showToast((e as Error).message, 'error')
  }
}
</script>

<style scoped>
.explore-view {
  height: 100%;
  min-height: 0;
  display: flex;
  flex-direction: column;
  background: var(--color-bg);
  overflow: hidden;
}

.explore-header {
  padding: 16px 24px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  border-bottom: 1px solid var(--color-border);
  background: var(--color-bg-elevated);
}

.explore-header h2 {
  font-size: var(--text-xl);
  font-weight: 700;
  margin: 0;
  color: var(--color-text);
}

.source-selector {
  min-width: 0;
}

.source-selector select {
  padding: 6px 12px;
  border-radius: 6px;
  border: 1px solid var(--color-border);
  background: var(--color-bg);
  color: var(--color-text);
  font-size: var(--text-base);
  outline: none;
  cursor: pointer;
  /* 书源名过长时截断省略 */
  max-width: min(320px, 52vw);
  text-overflow: ellipsis;
  white-space: nowrap;
  overflow: hidden;
}

.no-sources-text {
  font-size: var(--text-sm);
  color: var(--color-text-tertiary);
}

.explore-body {
  flex: 1;
  display: flex;
  overflow: hidden;
}

/* 侧边栏模式 (PC端) */
.categories-panel {
  width: 200px;
  border-right: 1px solid var(--color-border);
  background: var(--color-bg-elevated);
  overflow: auto;
  min-height: 0;
}

.categories-scroll {
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.category-tag {
  padding: 10px 16px;
  border-radius: 8px;
  cursor: pointer;
  font-size: var(--text-base);
  color: var(--color-text-secondary);
  transition: all 0.2s;
}

.category-tag.section {
  cursor: default;
  color: var(--color-text-tertiary);
}

.category-tag:hover:not(.section) {
  background: var(--color-bg-hover);
}

.category-tag.active {
  background: rgba(201, 127, 58, 0.1);
  color: var(--color-primary);
  font-weight: 600;
}

.content-panel {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 24px;
  position: relative;
}

.books-grid-wrapper {
  margin-bottom: 24px;
}

.load-more-sentinel {
  height: 1px;
}

.loading-state, .end-state, .error-state {
  text-align: center;
  padding: 20px 0;
  color: var(--color-text-tertiary);
  font-size: var(--text-base);
  display: flex;
  justify-content: center;
  align-items: center;
  gap: 8px;
}

.error-state {
  color: #ef4444;
}

.spinner {
  width: 16px;
  height: 16px;
  border: 2px solid var(--color-border);
  border-top-color: var(--color-primary);
  border-radius: 50%;
  animation: spin 1s linear infinite;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

/* 移动端适配 */
@media (max-width: 768px) {
  .explore-body {
    flex-direction: column;
  }
  
  .explore-header {
    padding: 12px 16px;
  }

  .categories-panel {
    width: 100%;
    border-right: none;
    border-bottom: 1px solid var(--color-border);
    overflow-x: auto;
    overflow-y: hidden;
    flex-shrink: 0;
  }

  .categories-scroll {
    flex-direction: row;
    padding: 8px 16px;
    gap: 8px;
  }

  .category-tag {
    white-space: nowrap;
    padding: 6px 14px;
    background: var(--color-bg);
    border: 1px solid var(--color-border);
    border-radius: 20px;
  }
  
  .category-tag.active {
    background: var(--color-primary);
    color: white;
    border-color: var(--color-primary);
  }

  .content-panel {
    padding: 16px;
  }
}
</style>
