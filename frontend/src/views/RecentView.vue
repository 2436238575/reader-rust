<template>
  <div class="recent-view">
    <div class="recent-content" ref="recentContentRef">
      <div class="recent-header">
        <h1 class="recent-title">
          最近阅读
          <span class="recent-count">({{ filteredRecentBooks.length }})</span>
        </h1>
        <div class="recent-actions">
          <button
            class="recent-clear-btn"
            :disabled="!shelfStore.recentBooks.length"
            @click="handleClearRecent"
          >
            一键清空
          </button>
        </div>
      </div>

      <BookGrid
        :books="filteredRecentBooks"
        :loading="shelfStore.loading"
        empty-text="暂无最近阅读"
        :show-delete-action="true"
        @click="handleBookClick"
        @info="handleBookInfo"
        @delete="handleRecentDelete"
        @ai="handleBookAi"
      />
    </div>

    <BookDetailModal v-model="showDetail" :book="selectedBook" />
  </div>
</template>

<script setup lang="ts">
import { computed, onActivated, ref } from 'vue'
import { usePreserveScroll } from '../composables/usePreserveScroll'
import { useRouter } from 'vue-router'
import BookDetailModal from '../components/BookDetailModal.vue'
import BookGrid from '../components/BookGrid.vue'
import { useBookshelfStore } from '../stores/bookshelf'
import { useAppStore } from '../stores/app'
import { useOpenBook } from '../composables/useOpenBook'
import type { Book, SearchBook } from '../types'

const router = useRouter()
const shelfStore = useBookshelfStore()
const appStore = useAppStore()
const { openBook } = useOpenBook()

const showDetail = ref(false)
const selectedBook = ref<Book | SearchBook | null>(null)

// 滚动容器是 BookGrid 根元素（.recent-content :deep(.book-grid)）；
// 保活恢复时 Chromium 已丢失其滚动位置，手动回填
const recentContentRef = ref<HTMLElement>()
usePreserveScroll(() => recentContentRef.value?.querySelector('.book-grid') as HTMLElement | null)

const filteredRecentBooks = computed(() => {
  const list = shelfStore.recentBooks

  // 搜索复用顶栏搜索框：关键词由 AppTopBar 写入 recentFilter
  const keyword = shelfStore.recentFilter.trim().toLowerCase()
  if (!keyword) return list

  return list.filter((book) =>
    [
      book.name,
      book.author,
      book.intro,
      book.originName,
      book.origin,
      book.latestChapterTitle,
      book.durChapterTitle,
    ]
      .filter(Boolean)
      .some((value) => String(value).toLowerCase().includes(keyword))
  )
})

// 保活页面：onActivated 在首次挂载与每次返回时都触发——
// 从阅读器返回后最近阅读排序/进度已变，必须重拉
onActivated(async () => {
  await shelfStore.fetchBooks().catch(() => undefined)
  await shelfStore.refreshRecentBooks().catch((error) => {
    // 加载失败不能静默：否则用户看到「暂无最近阅读」的误导性空态
    appStore.showToast((error as Error)?.message || '最近阅读加载失败', 'error')
  })
})

async function handleBookClick(book: Book | SearchBook) {
  await openBook(book)
}

function handleBookInfo(book: Book | SearchBook) {
  selectedBook.value = book
  showDetail.value = true
}

async function handleRecentDelete(book: Book | SearchBook) {
  try {
    await shelfStore.removeRecentBook(book as Book)
    appStore.showToast(`已删除 "${book.name}"`, 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || '删除失败', 'error')
  }
}

function handleBookAi(book: Book | SearchBook) {
  router.push({
    name: 'ai-book',
    query: { bookUrl: book.bookUrl },
  })
}

async function handleClearRecent() {
  if (!confirm('确定清空最近阅读记录吗？')) return
  try {
    await shelfStore.clearAllRecentBooks()
    appStore.showToast('已清空最近阅读', 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || '清空失败', 'error')
  }
}
</script>

<style scoped>
.recent-view {
  height: 100%;
  min-height: 0;
  overflow: hidden;
}

.recent-content {
  height: 100%;
  max-width: var(--content-max-width);
  margin: 0 auto;
  padding: 0 var(--space-6);
  display: flex;
  flex-direction: column;
  min-height: 0;
}

.recent-header {
  padding: var(--space-6) 0 var(--space-3);
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-4);
  flex-shrink: 0;
}

.recent-content :deep(.book-grid) {
  flex: 1;
  min-height: 0;
  overflow: auto;
}

.recent-title {
  font-size: var(--text-xl);
  font-weight: 700;
  letter-spacing: -0.02em;
}

.recent-count {
  font-size: var(--text-base);
  font-weight: 400;
  color: var(--color-text-tertiary);
}

.recent-actions {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  justify-content: flex-end;
}

.recent-clear-btn {
  padding: 10px 16px;
  border-radius: 999px;
  border: 1px solid var(--color-border-light);
  background: var(--color-bg-elevated);
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
  transition: all var(--duration-fast) var(--ease-out);
}

.recent-clear-btn:hover:not(:disabled) {
  border-color: rgba(225, 76, 76, 0.22);
  color: var(--color-danger);
}

.recent-clear-btn:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

@media (max-width: 640px) {
  .recent-header {
    align-items: flex-start;
    flex-direction: column;
  }

  .recent-actions {
    width: 100%;
    justify-content: space-between;
  }
}
</style>
