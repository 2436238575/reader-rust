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
                    <span v-if="(book as Book).totalChapterNum" class="tag"
                      >共{{ (book as Book).totalChapterNum }}章</span
                    >
                    <button
                      v-if="book && book.origin"
                      class="tag origin tag-switch"
                      type="button"
                      title="点击切换书源"
                      @click="showSourcePanel = true"
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M16 3l4 4-4 4" />
                        <path d="M20 7H7" />
                        <path d="M8 21l-4-4 4-4" />
                        <path d="M4 17h13" />
                      </svg>
                      {{ book.originName || book.origin }}
                    </button>
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
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  width="18"
                  height="18"
                >
                  <path d="M12 2v4" />
                  <path d="M12 18v4" />
                  <path d="M2 12h4" />
                  <path d="M18 12h4" />
                  <circle cx="12" cy="12" r="3" />
                </svg>
                AI资料
              </button>
              <button class="btn btn-primary detail-btn" @click="startReading">
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  width="18"
                  height="18"
                >
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

          <!-- 换源面板：盖在详情内容之上，入口是书籍信息里的来源标签 -->
          <Transition :name="isMobileLayout ? 'slide-right' : 'scale'">
            <BookSourceSwitchPanel
              v-if="showSourcePanel && book"
              class="source-switch-layer"
              :book="book"
              :switching="switchingSource"
              @switch="handleSourceSwitch"
              @close="showSourcePanel = false"
            />
          </Transition>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { ref, watch, computed, toRef } from 'vue'
import { useEscClose } from '../composables/useEscClose'
import { useRouter } from 'vue-router'
import { getCoverUrl, getChapterList, setBookSource } from '../api/bookshelf'
import { getBrowserCatalog, setBrowserCatalog } from '../utils/browserCache'
import { useMobileLayout } from '../composables/useMobileLayout'
import { useOpenBook } from '../composables/useOpenBook'
import { useBookshelfStore } from '../stores/bookshelf'
import { useAppStore } from '../stores/app'
import { useSourceNameResolver } from '../composables/useAvailableSources'
import BookSourceSwitchPanel from './BookSourceSwitchPanel.vue'
import type { Book, SearchBook, BookChapter } from '../types'

const props = defineProps<{
  modelValue: boolean
  book: Book | SearchBook | null
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
  switched: [book: Book | SearchBook]
}>()

useEscClose(toRef(props, 'modelValue'), () => emit('update:modelValue', false))

const router = useRouter()
const { openBook } = useOpenBook()
const { isMobileLayout } = useMobileLayout()
const shelfStore = useBookshelfStore()
const appStore = useAppStore()
const resolveSourceName = useSourceNameResolver()

// 弹窗内的工作副本：换源后指向新书；重开弹窗时从 props 重新取，
// 父组件的 selectedBook 不必跟着换源结果走
const book = ref<Book | SearchBook | null>(null)
const showSourcePanel = ref(false)
const switchingSource = ref(false)

const coverFailed = ref(false)
const chapters = ref<BookChapter[]>([])
const chaptersLoading = ref(false)
const chaptersFailed = ref(false)
const showAllChapters = ref(false)

const coverSrc = computed(() => {
  if (coverFailed.value || !book.value) return ''
  const url = (book.value as Book).customCoverUrl || book.value.coverUrl
  return url ? getCoverUrl(url) : ''
})

const displayChapters = computed(() => {
  if (showAllChapters.value) return chapters.value
  return chapters.value.slice(0, 50)
})

async function loadChapters() {
  if (!book.value) return
  chaptersFailed.value = false
  chaptersLoading.value = true
  try {
    const b = book.value as Book
    const list = await getChapterList({
      bookUrl: b.bookUrl,
      bookSourceUrl: b.origin,
    })
    chapters.value = list
    // 落浏览器目录缓存：离线时详情弹窗与阅读器都能回退
    void setBrowserCatalog(b.bookUrl, list)
  } catch {
    // 离线兜底：先翻浏览器里的目录缓存，实在没有才报失败
    const cached = await getBrowserCatalog<BookChapter>((book.value as Book).bookUrl)
    if (cached) {
      chapters.value = cached
    } else {
      // 此前静默吞掉：目录区直接空白，用户分不清「没目录」还是「加载失败」
      chapters.value = []
      chaptersFailed.value = true
    }
  } finally {
    chaptersLoading.value = false
  }
}

watch(
  () => props.modelValue,
  async (visible) => {
    if (visible && props.book) {
      book.value = props.book
      showSourcePanel.value = false
      coverFailed.value = false
      showAllChapters.value = false
      chapters.value = []
      await loadChapters()
    }
  }
)

function close() {
  emit('update:modelValue', false)
}

async function startReading() {
  if (!book.value) return
  const current = book.value
  close()
  await openBook(current)
}

async function readChapter(index: number) {
  if (!book.value) return
  const current = book.value
  close()
  await openBook(current, index)
}

function openAiBook() {
  if (!book.value) return
  const b = book.value as Book
  close()
  router.push({
    name: 'ai-book',
    query: { bookUrl: b.bookUrl },
  })
}

/* ─── 换源 ─── */

// 换源后新目录的章节序号可能整体漂移：按已读章节标题就近对齐，
// 对不上就夹回合法范围（阅读页对越界序号不动作，「继续阅读」会卡住）
function alignDurChapterAfterSwitch(target: Book, list: BookChapter[]) {
  if (!list.length) return
  const normalize = (title?: string) =>
    (title || '')
      .replace(/\s+/g, '')
      .replace(/[^\p{L}\p{N}]/gu, '')
      .toLowerCase()
  const wanted = normalize(target.durChapterTitle)
  let index = -1
  if (wanted) {
    index = list.findIndex((chapter) => normalize(chapter.title) === wanted)
    if (index < 0) {
      index = list.findIndex((chapter) => {
        const title = normalize(chapter.title)
        return title.includes(wanted) || wanted.includes(title)
      })
    }
  }
  target.durChapterIndex =
    index >= 0 ? index : Math.max(0, Math.min(list.length - 1, target.durChapterIndex ?? 0))
}

async function handleSourceSwitch(candidate: SearchBook) {
  const current = book.value
  if (!current || switchingSource.value) return
  switchingSource.value = true
  try {
    let switched: Book | SearchBook
    if (shelfStore.books.some((item) => item.bookUrl === current.bookUrl)) {
      // 书架书：后端换源落库（不依赖旧书源仍存在），返回换源后的完整书籍
      const updated = await setBookSource({
        bookUrl: current.bookUrl,
        newUrl: candidate.bookUrl,
        bookSourceUrl: candidate.origin,
      })
      book.value = updated
      switched = updated
      // 书架网格还是旧源信息：后台刷新，失败不打断换源本身
      void shelfStore.fetchBooks().catch(() => undefined)
    } else {
      // 搜索/发现结果未入库：换源只是把详情指向另一源的这本书
      switched = { ...candidate, latestChapterTitle: candidate.lastChapter }
      book.value = switched
    }

    showSourcePanel.value = false
    coverFailed.value = false
    appStore.showToast(
      `已切换到 ${switched.originName || resolveSourceName(candidate.origin)}`,
      'success'
    )
    await loadChapters()
    if ('durChapterTitle' in switched) {
      alignDurChapterAfterSwitch(switched, chapters.value)
    }
    // 通知宿主换源结果（阅读页用它刷新当前会话；其余宿主可忽略）
    emit('switched', switched)
  } catch (e: any) {
    appStore.showToast(`切换失败: ${e?.message || '未知错误'}`, 'error')
  } finally {
    switchingSource.value = false
  }
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
  padding: calc(var(--space-6) + var(--safe-area-top)) calc(var(--space-6) + var(--safe-area-right))
    calc(var(--space-6) + var(--safe-area-bottom)) calc(var(--space-6) + var(--safe-area-left));
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
}

.detail-modal {
  width: 100%;
  max-width: 880px;
  max-height: min(
    85vh,
    calc(var(--app-height, 100dvh) - var(--safe-area-top) - var(--safe-area-bottom) - 32px)
  );
  display: flex;
  flex-direction: column;
  background: var(--color-bg-elevated);
  border-radius: var(--radius-xl);
  padding: var(--space-8);
  position: relative;
  box-shadow: var(--shadow-xl);
  overflow: hidden;
}

/* 换源面板：盖住详情内容（父容器 overflow:hidden 负责裁圆角） */
.source-switch-layer {
  position: absolute;
  inset: 0;
  z-index: 5;
  background: var(--color-bg-elevated);
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

/* 来源标签即换源入口：常驻可点，悬停变实底提示可交互 */
.tag-switch {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  max-width: 220px;
  transition:
    background var(--duration-fast),
    color var(--duration-fast);
}

.tag-switch svg {
  width: 11px;
  height: 11px;
  flex-shrink: 0;
}

.tag-switch:hover {
  background: var(--color-primary);
  color: var(--color-bg-elevated);
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
  to {
    transform: rotate(360deg);
  }
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
    padding: var(--safe-area-top) 0 var(--safe-area-bottom) 0;
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
