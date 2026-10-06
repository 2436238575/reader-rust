<template>
  <div class="source-switch">
    <div class="switch-header">
      <div class="header-left">
        <h3>切换书源</h3>
        <span v-if="preparedResults.length" class="source-count"
          >{{ preparedResults.length }} 个结果</span
        >
      </div>
      <button class="close-btn" @click="emit('close')">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M18 6 6 18M6 6l12 12" />
        </svg>
      </button>
    </div>

    <div class="switch-body">
      <div class="current-brief">
        <div class="brief-main">
          <div class="brief-title">{{ book.name }}</div>
          <div class="brief-meta">{{ book.author || '未知作者' }}</div>
        </div>
        <div class="brief-source">
          <span class="brief-label">当前书源</span>
          <span class="brief-value">{{ currentSourceName }}</span>
        </div>
      </div>

      <div class="section-label">其他可用源</div>

      <div v-if="searching && !preparedResults.length" class="state-hint">
        <div class="loading-spinner"></div>
        正在全网搜索同名书籍...
      </div>

      <div v-else-if="!preparedResults.length" class="state-hint">未找到其他书源</div>

      <div
        v-else
        v-for="item in preparedResults"
        :key="item.book.bookUrl + item.book.origin"
        class="candidate"
        @click="emit('switch', item.book)"
      >
        <div class="candidate-main">
          <div class="candidate-name-row">
            <span class="candidate-name">{{ resolveSourceName(item.book.origin) }}</span>
            <span v-if="item.sameLatest" class="match-badge good">最新章节一致</span>
            <span v-else-if="item.book.lastChapter" class="match-badge">章节不同</span>
          </div>
          <div v-if="item.book.name !== book.name" class="candidate-meta">{{ item.book.name }}</div>
          <div v-if="item.book.lastChapter" class="candidate-meta">
            最新: {{ item.book.lastChapter }}
          </div>
          <div v-if="item.chapterHint && !item.sameLatest" class="candidate-hint">
            {{ item.chapterHint }}
          </div>
        </div>
        <button class="switch-btn" @click.stop="emit('switch', item.book)">切换</button>
      </div>

      <div v-if="searching && preparedResults.length" class="inline-loading">
        <div class="loading-spinner small"></div>
        继续搜索其他书源...
      </div>

      <div v-if="hasMoreSources && !searching && preparedResults.length" class="load-more-wrap">
        <button class="load-more-btn" :disabled="loadingMore" @click="loadMoreSources">
          {{ loadingMore ? '加载中...' : '加载更多' }}
        </button>
      </div>
    </div>

    <Transition name="fade">
      <div v-if="switching" class="switch-overlay">
        <div class="loading-spinner"></div>
        <p>正在切换书源，请稍候...</p>
      </div>
    </Transition>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useAvailableSources, useSourceNameResolver } from '../composables/useAvailableSources'
import { useSourceStore } from '../stores/source'
import { useEscClose } from '../composables/useEscClose'
import type { Book, SearchBook } from '../types'

const props = defineProps<{
  book: Book | SearchBook
  switching: boolean
}>()

const emit = defineEmits<{
  switch: [candidate: SearchBook]
  close: []
}>()

// 独立弹层：ESC 只关面板，宿主详情弹窗留给下一次 ESC
useEscClose(ref(true), () => emit('close'))

const sourceStore = useSourceStore()
const resolveSourceName = useSourceNameResolver()

const { searching, loadingMore, hasMoreSources, preparedResults, startSearch, loadMoreSources } =
  useAvailableSources(() => {
    const b = props.book
    return {
      bookUrl: b.bookUrl,
      name: b.name,
      author: b.author,
      origin: b.origin,
      latest: (b as Book).latestChapterTitle || (b as SearchBook).lastChapter,
    }
  })

const currentSourceName = computed(
  () => (props.book as Book).originName || resolveSourceName(props.book.origin)
)

onMounted(() => {
  // 候选源名要靠书源库翻译；书架页直接开详情弹窗时可能还没拉过
  if (!sourceStore.sources.length) {
    void sourceStore.fetchSources().catch(() => undefined)
  }
  startSearch()
})
</script>

<style scoped>
.source-switch {
  display: flex;
  flex-direction: column;
  height: 100%;
  position: relative;
}

.switch-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--space-4) var(--space-5);
  border-bottom: 1px solid var(--color-divider);
  flex-shrink: 0;
}

.header-left {
  display: flex;
  align-items: baseline;
  gap: var(--space-2);
}

.switch-header h3 {
  font-size: var(--text-lg);
  margin: 0;
}

.source-count {
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.close-btn {
  width: 32px;
  height: 32px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--radius-md);
  color: var(--color-text-tertiary);
}

.close-btn:hover {
  background: var(--color-bg-hover);
  color: var(--color-text);
}

.close-btn svg {
  width: 18px;
  height: 18px;
}

.switch-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
}

.current-brief {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-4);
  margin: var(--space-4) var(--space-5) 0;
  padding: var(--space-3) var(--space-4);
  border-radius: var(--radius-lg);
  background: var(--color-primary-bg);
}

.brief-main {
  min-width: 0;
}

.brief-title {
  font-size: var(--text-base);
  font-weight: 700;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.brief-meta {
  font-size: var(--text-xs);
  color: var(--color-text-secondary);
  margin-top: 2px;
}

.brief-source {
  flex-shrink: 0;
  max-width: 45%;
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 2px;
}

.brief-label {
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.brief-value {
  font-size: var(--text-xs);
  color: var(--color-primary);
  font-weight: 600;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.section-label {
  padding: var(--space-4) var(--space-5) var(--space-1);
  font-size: var(--text-xs);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--color-text-tertiary);
  font-weight: 600;
}

.candidate {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  padding: var(--space-3) var(--space-5);
  border-bottom: 1px solid var(--color-divider);
  cursor: pointer;
  transition: background var(--duration-fast);
}

.candidate:hover {
  background: var(--color-bg-hover);
}

.candidate-main {
  flex: 1;
  min-width: 0;
}

.candidate-name-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
}

.candidate-name {
  font-size: var(--text-sm);
  font-weight: 600;
  color: var(--color-text);
}

.match-badge {
  font-size: var(--text-xs);
  padding: 1px var(--space-2);
  border-radius: 999px;
  background: var(--color-bg-sunken);
  color: var(--color-text-tertiary);
}

.match-badge.good {
  background: var(--color-primary-bg);
  color: var(--color-primary);
}

.candidate-meta {
  font-size: var(--text-xs);
  color: var(--color-text-secondary);
  margin-top: 2px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.candidate-hint {
  font-size: var(--text-xs);
  color: var(--color-primary);
  margin-top: 2px;
}

.switch-btn {
  flex-shrink: 0;
  padding: 4px 14px;
  font-size: var(--text-xs);
  border-radius: 999px;
  border: 1px solid var(--color-border);
  background: transparent;
  color: var(--color-text-secondary);
}

.candidate:hover .switch-btn {
  background: var(--color-primary);
  border-color: var(--color-primary);
  color: var(--color-bg-elevated);
}

.state-hint {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-2);
  padding: var(--space-8) var(--space-5);
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
}

.inline-loading {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-2);
  padding: var(--space-3) var(--space-5);
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.load-more-wrap {
  padding: var(--space-4) var(--space-5) var(--space-6);
  display: flex;
  justify-content: center;
}

.load-more-btn {
  min-width: 120px;
  padding: var(--space-2) var(--space-4);
  border-radius: 999px;
  border: 1px solid var(--color-border);
  background: transparent;
  color: var(--color-text-secondary);
}

.load-more-btn:hover:not(:disabled) {
  background: var(--color-primary-bg);
  border-color: var(--color-primary);
  color: var(--color-primary);
}

.load-more-btn:disabled {
  opacity: 0.5;
  cursor: wait;
}

.loading-spinner {
  width: 20px;
  height: 20px;
  border: 2px solid var(--color-border);
  border-top-color: var(--color-primary);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
  flex-shrink: 0;
}

.loading-spinner.small {
  width: 14px;
  height: 14px;
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

.switch-overlay {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: var(--space-3);
  background: var(--color-bg-elevated);
  z-index: 2;
}

.switch-overlay p {
  margin: 0;
  font-size: var(--text-sm);
  color: var(--color-text-secondary);
}

@media (max-width: 767px) {
  .switch-header {
    padding: var(--space-3) var(--space-4);
  }

  .current-brief {
    margin: var(--space-3) var(--space-4) 0;
  }

  .candidate {
    padding: var(--space-3) var(--space-4);
  }

  .section-label {
    padding: var(--space-3) var(--space-4) var(--space-1);
  }
}
</style>
