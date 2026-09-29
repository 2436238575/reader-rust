<template>
  <Teleport to="body">
    <Transition name="fade">
      <div v-if="modelValue" class="modal-overlay" @click="close"></div>
    </Transition>
    <Transition :name="isMobileLayout ? 'slide-right' : 'scale'">
      <div v-if="modelValue && book" class="modal-container" @click.self="close">
        <div class="detail-modal" :class="{ 'has-long-toc': chapters.length > 14 }">
          <button class="modal-close" @click="close">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>

          <div class="detail-body">
            <!-- 左栏：书籍信息 + 简介 -->
            <div class="detail-left">
              <div class="book-header">
                <div class="book-cover-lg">
                  <img
                    v-if="coverSrc"
                    :src="coverSrc"
                    :alt="book.name"
                    @error="coverFailed = true"
                  />
                  <div v-else class="cover-placeholder-lg">
                    <span>{{ book.name }}</span>
                  </div>
                </div>
                <div class="book-header-info">
                  <h2>{{ book.name }}</h2>
                  <p class="author">{{ book.author || '未知作者' }}</p>
                  <div class="book-tags">
                    <span v-if="book.kind" class="tag">{{ book.kind }}</span>
                    <span v-if="(book as Book).totalChapterNum" class="tag">共{{ (book as Book).totalChapterNum }}章</span>
                    <span v-if="(book as Book).originName" class="tag origin">{{ (book as Book).originName }}</span>
                  </div>
                  <p v-if="(book as Book).durChapterTitle" class="progress">
                    已读至：{{ (book as Book).durChapterTitle }}
                  </p>
                </div>
              </div>

              <!-- Intro -->
              <div v-if="book.intro" class="book-intro">
                <h3>简介</h3>
                <p>{{ book.intro }}</p>
              </div>
            </div>

            <!-- 右栏：目录 -->
            <div class="detail-right">
              <div class="chapter-section" v-if="chapters.length > 0">
                <h3>目录 ({{ chapters.length }})</h3>
                <div class="chapter-list">
                  <div
                    v-for="(chapter, i) in displayChapters"
                    :key="chapter.url"
                    class="chapter-item"
                    :class="{ current: i === (book as Book).durChapterIndex }"
                    @click="readChapter(i)"
                  >
                    <span class="chapter-index">{{ i + 1 }}</span>
                    <span class="chapter-title">{{ chapter.title }}</span>
                  </div>
                </div>
                <button
                  v-if="chapters.length > 50 && !showAllChapters"
                  class="show-more-btn"
                  @click="showAllChapters = true"
                >
                  显示全部 {{ chapters.length }} 章
                </button>
              </div>
              <div v-else-if="chaptersLoading" class="chapter-loading">
                <div class="loading-spinner"></div>
                加载目录中...
              </div>
              <div v-else-if="chaptersFailed" class="chapter-failed">
                目录加载失败
                <button class="chapter-retry-btn" @click="loadChapters">重试</button>
              </div>
            </div>
          </div>

          <!-- Actions -->
          <div class="modal-actions">
            <div class="actions-left">
              <button class="btn detail-btn" @click="openAiBook">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="18" height="18">
                  <path d="M12 2v4" />
                  <path d="M12 18v4" />
                  <path d="M2 12h4" />
                  <path d="M18 12h4" />
                  <circle cx="12" cy="12" r="3" />
                </svg>
                AI资料
              </button>
              <button class="btn btn-primary detail-btn" @click="startReading">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="18" height="18">
                  <path d="M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z" />
                  <path d="M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z" />
                </svg>
                {{ (book as Book).durChapterIndex ? '继续阅读' : '开始阅读' }}
              </button>
            </div>
            <!-- 桌面端放在右栏位与「开始阅读」同一行对齐；移动端隐藏，用目录内的同名按钮 -->
            <div v-if="chapters.length > 50 && !showAllChapters" class="actions-right">
              <button class="show-more-btn" @click="showAllChapters = true">
                显示全部 {{ chapters.length }} 章
              </button>
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { ref, watch, computed, toRef } from 'vue'
import { useEscClose } from '../composables/useEscClose'
import { useRouter } from 'vue-router'
import { getCoverUrl, getChapterList } from '../api/bookshelf'
import { useMobileLayout } from '../composables/useMobileLayout'
import { useOpenBook } from '../composables/useOpenBook'
import type { Book, SearchBook, BookChapter } from '../types'

const props = defineProps<{
  modelValue: boolean
  book: Book | SearchBook | null
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
}>()

useEscClose(toRef(props, 'modelValue'), () => emit('update:modelValue', false))

const router = useRouter()
const { openBook } = useOpenBook()
const { isMobileLayout } = useMobileLayout()

const coverFailed = ref(false)
const chapters = ref<BookChapter[]>([])
const chaptersLoading = ref(false)
const chaptersFailed = ref(false)
const showAllChapters = ref(false)

const coverSrc = computed(() => {
  if (coverFailed.value || !props.book) return ''
  const url = (props.book as Book).customCoverUrl || props.book.coverUrl
  return url ? getCoverUrl(url) : ''
})

const displayChapters = computed(() => {
  if (showAllChapters.value) return chapters.value
  return chapters.value.slice(0, 50)
})

async function loadChapters() {
  if (!props.book) return
  chaptersFailed.value = false
  chaptersLoading.value = true
  try {
    const b = props.book as Book
    chapters.value = await getChapterList({
      bookUrl: b.bookUrl,
      bookSourceUrl: b.origin,
    })
  } catch {
    // 此前静默吞掉：目录区直接空白，用户分不清「没目录」还是「加载失败」
    chapters.value = []
    chaptersFailed.value = true
  } finally {
    chaptersLoading.value = false
  }
}

watch(() => props.modelValue, async (visible) => {
  if (visible && props.book) {
    coverFailed.value = false
    showAllChapters.value = false
    chapters.value = []
    await loadChapters()
  }
})

function close() {
  emit('update:modelValue', false)
}

async function startReading() {
  if (!props.book) return
  const book = props.book
  close()
  await openBook(book)
}

async function readChapter(index: number) {
  if (!props.book) return
  const book = props.book
  close()
  await openBook(book, index)
}

function openAiBook() {
  if (!props.book) return
  const b = props.book as Book
  close()
  router.push({
    name: 'ai-book',
    query: { bookUrl: b.bookUrl },
  })
}
</script>

<style scoped>
.modal-overlay {
  position: fixed;
  inset: 0;
  background: var(--overlay-mask-bg);
  z-index: var(--z-overlay);
  -webkit-backdrop-filter: var(--overlay-mask-blur);
  backdrop-filter: var(--overlay-mask-blur);
}

.modal-container {
  position: fixed;
  inset: 0;
  z-index: var(--z-modal);
  display: flex;
  align-items: center;
  justify-content: center;
  padding:
    calc(var(--space-6) + var(--safe-area-top))
    calc(var(--space-6) + var(--safe-area-right))
    calc(var(--space-6) + var(--safe-area-bottom))
    calc(var(--space-6) + var(--safe-area-left));
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
}

.detail-modal {
  width: 100%;
  max-width: 880px;
  max-height: min(85vh, calc(var(--app-height, 100dvh) - var(--safe-area-top) - var(--safe-area-bottom) - 32px));
  display: flex;
  flex-direction: column;
  background: var(--color-bg-elevated);
  border-radius: var(--radius-xl);
  padding: var(--space-8);
  position: relative;
  box-shadow: var(--shadow-xl);
  overflow: hidden;
}

.detail-body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: var(--space-5);
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
}

.detail-left {
  min-width: 0;
}

.detail-right {
  min-width: 0;
}

.modal-close {
  position: absolute;
  top: max(var(--space-4), calc(var(--safe-area-top) * 0.35));
  right: var(--space-4);
  width: 32px;
  height: 32px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--radius-md);
  color: var(--color-text-tertiary);
  transition: all var(--duration-fast);
  z-index: 1;
}

.modal-close:hover {
  background: var(--color-bg-hover);
  color: var(--color-text);
}

.modal-close svg {
  width: 18px;
  height: 18px;
}

.book-header {
  display: flex;
  gap: var(--space-5);
  margin-bottom: var(--space-6);
}

.book-cover-lg {
  width: 120px;
  height: 160px;
  flex-shrink: 0;
  border-radius: var(--radius-md);
  overflow: hidden;
  background: var(--color-bg-sunken);
  box-shadow: var(--shadow-md);
}

.book-cover-lg img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.cover-placeholder-lg {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, var(--color-primary-bg), var(--color-bg-sunken));
  padding: var(--space-3);
  text-align: center;
  font-size: var(--text-sm);
  font-weight: 600;
  color: var(--color-primary);
}

.book-header-info {
  flex: 1;
  min-width: 0;
}

.book-header-info h2 {
  font-size: var(--text-xl);
  font-weight: 700;
  margin-bottom: var(--space-2);
  line-height: var(--leading-tight);
}

.author {
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
  margin-bottom: var(--space-3);
}

.book-tags {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  margin-bottom: var(--space-3);
}

.tag {
  padding: 2px var(--space-2);
  background: var(--color-bg-sunken);
  border-radius: var(--radius-sm);
  font-size: var(--text-xs);
  color: var(--color-text-secondary);
}

.tag.origin {
  background: var(--color-primary-bg);
  color: var(--color-primary);
}

.progress {
  font-size: var(--text-sm);
  color: var(--color-primary);
}

.book-intro {
  margin-bottom: var(--space-2);
}

.book-intro h3 {
  font-size: var(--text-base);
  font-weight: 600;
  margin-bottom: var(--space-2);
}

.book-intro p {
  font-size: var(--text-sm);
  color: var(--color-text-secondary);
  line-height: var(--leading-relaxed);
  white-space: pre-wrap;
}

.chapter-section h3 {
  font-size: var(--text-base);
  font-weight: 600;
  margin-bottom: var(--space-3);
}

.chapter-list {
  border: 1px solid var(--color-border-light);
  border-radius: var(--radius-md);
}

.chapter-item {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-2) var(--space-3);
  cursor: pointer;
  transition: background var(--duration-fast);
  font-size: var(--text-sm);
  border-bottom: 1px solid var(--color-divider);
}

.chapter-item:last-child {
  border-bottom: none;
}

.chapter-item:hover {
  background: var(--color-bg-hover);
}

.chapter-item.current {
  color: var(--color-primary);
  background: var(--color-primary-bg);
}

.chapter-index {
  color: var(--color-text-tertiary);
  font-size: var(--text-xs);
  min-width: 28px;
}

.chapter-title {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.show-more-btn {
  width: 100%;
  padding: var(--space-3);
  text-align: center;
  color: var(--color-primary);
  font-size: var(--text-sm);
  font-weight: 500;
  margin-top: var(--space-2);
  border-radius: var(--radius-md);
  transition: background var(--duration-fast);
}

.show-more-btn:hover {
  background: var(--color-primary-bg);
}

.chapter-loading {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-2);
  padding: var(--space-6);
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
}

.chapter-failed {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-3);
  padding: var(--space-6);
  color: var(--color-danger);
  font-size: var(--text-sm);
}

.chapter-retry-btn {
  padding: 2px 14px;
  border-radius: var(--radius-md);
  border: 1px solid currentColor;
  background: transparent;
  color: inherit;
  font-size: var(--text-xs);
  cursor: pointer;
}

.loading-spinner {
  width: 18px;
  height: 18px;
  border: 2px solid var(--color-border);
  border-top-color: var(--color-primary);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

.modal-actions {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  margin-top: var(--space-5);
  padding-top: var(--space-5);
  border-top: 1px solid var(--color-divider);
  flex-shrink: 0;
}

.actions-left {
  display: flex;
  gap: var(--space-3);
}

.actions-left .detail-btn {
  flex: 1;
  min-height: 38px;
}

/* 「显示全部」在目录区内另有一份；这里的一份只给桌面端右栏位用 */
.actions-right {
  display: none;
}

/* 桌面端：左右分栏，卡片整体不滚动。弹窗高度由内容决定（简介可撑大），
   目录超过 14 章且视口放得下时垫高到至少露 14.5 行（174px ≈ 目录标题、
   操作区与上下内边距）；装不下则目录自身滚动，不硬塞。 */
@media (min-width: 768px) {
  .detail-modal.has-long-toc {
    min-height: min(85vh, calc(37px * 14.5 + 174px));
  }

  .detail-body {
    flex-direction: row;
    overflow: visible;
    gap: var(--space-6);
  }

  .detail-left {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  /* 「简介」标题固定，只有正文滚动 */
  .book-intro {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .book-intro p {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    padding-right: var(--space-2);
  }

  .detail-right {
    width: 300px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  .chapter-section {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }

  .chapter-list {
    overflow-y: auto;
    -webkit-overflow-scrolling: touch;
  }

  /* 长目录不传导高度：弹窗高度只由左侧内容（含简介完整显示所需）决定，
     目录在右栏内自适应填充、装不下自行滚动 */
  .has-long-toc .chapter-list {
    height: 0;
    flex: 1 1 auto;
    min-height: 0;
  }

  .chapter-section > .show-more-btn {
    display: none;
  }

  .modal-actions {
    flex-direction: row;
    gap: var(--space-6);
  }

  .actions-left {
    flex: 1;
  }

  /* 「显示全部」落在右栏正下方，与左栏的「开始阅读」同一行 */
  .actions-right {
    display: block;
    width: 300px;
  }

  .actions-right .show-more-btn {
    margin-top: 0;
  }
}

/* 移动端：整屏面板，正文整体滚动，操作栏固定底部 */
@media (max-width: 767px) {
  .modal-overlay {
    display: none;
  }

  .modal-container {
    padding:
      var(--safe-area-top)
      0
      var(--safe-area-bottom)
      0;
  }

  .detail-modal {
    height: 100%;
    max-height: none;
    border-radius: 0;
    padding: calc(var(--space-6) + var(--safe-area-top)) var(--space-5) var(--space-4);
    box-shadow: none;
  }

  .chapter-list {
    border: none;
    border-radius: 0;
  }
}
</style>
