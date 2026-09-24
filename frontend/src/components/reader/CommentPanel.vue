<template>
  <Teleport to="body">
    <Transition name="comment-panel">
      <div v-if="show" class="comment-mask" @click.self="$emit('close')">
        <div class="comment-panel" :style="{ background: theme.popup, color: theme.fontColor }">
          <div class="comment-head">
            <div class="comment-title">
              <span>{{ mode === 'para' ? '段评' : '本章评论' }}</span>
              <span v-if="total > 0" class="comment-total">· {{ total }}</span>
            </div>
            <button class="comment-close" aria-label="关闭" @click="$emit('close')">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
            </button>
          </div>

          <blockquote v-if="mode === 'para' && paraText" class="comment-quote">
            {{ paraText }}
          </blockquote>

          <div ref="listRef" class="comment-list">
            <div v-if="loading && !items.length" class="comment-hint">加载中...</div>
            <div v-else-if="error" class="comment-hint comment-error">{{ error }}</div>
            <div v-else-if="!items.length" class="comment-hint">还没有评论</div>

            <article v-for="(item, index) in items" :key="item.id || `c-${index}`" class="comment-item">
              <div class="comment-meta">
                <span class="comment-name">{{ item.name || '匿名读者' }}</span>
                <span class="comment-time">{{ formatReviewTime(item.time) }}</span>
              </div>
              <p v-if="item.content" class="comment-text">{{ item.content }}</p>
              <div v-if="item.images.length" class="comment-images">
                <img
                  v-for="(url, imageIndex) in item.images"
                  :key="imageIndex"
                  :src="url"
                  alt=""
                  loading="lazy"
                  referrerpolicy="no-referrer"
                  @click="preview = url"
                >
              </div>
              <div v-if="item.replies.length" class="comment-replies">
                <p v-for="(reply, replyIndex) in item.replies" :key="replyIndex" class="comment-reply">
                  <span class="comment-reply-name">{{ reply.name || '匿名读者' }}</span>
                  <span v-if="reply.replyTo" class="comment-reply-to">回复 {{ reply.replyTo }}</span>
                  ：{{ reply.content }}
                </p>
              </div>
              <div v-if="item.replyCount > item.replies.length" class="comment-reply-more">
                共 {{ item.replyCount }} 条回复
              </div>
              <div class="comment-actions">
                <span class="comment-digg">
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                    <path d="M7 22V11l5-9a2 2 0 0 1 2 2v5h5.5a2 2 0 0 1 2 2.3l-1.4 8A2 2 0 0 1 18 22H7z" />
                  </svg>
                  {{ item.digg }}
                </span>
              </div>
            </article>

            <button v-if="hasMore" class="comment-more" :disabled="loading" @click="loadMore">
              {{ loading ? '加载中...' : '加载更多' }}
            </button>
          </div>
        </div>
      </div>
    </Transition>

    <div v-if="preview" class="comment-preview" @click="preview = ''">
      <img :src="preview" alt="" referrerpolicy="no-referrer">
    </div>
  </Teleport>
</template>

<script setup lang="ts">
import { nextTick, ref, watch } from 'vue'
import type { ReviewItem, ReviewPage } from '../../types'
import type { ThemePreset } from '../../stores/reader'
import { getChapterComments, getParaComments } from '../../api/review'

/** 浏览器能直接渲染的图片后缀；HEIC/HEIF 只有 Safari 认，排到最后。 */
const RENDERABLE_IMAGE_EXTENSIONS = [
  'jpg',
  'jpeg',
  'png',
  'gif',
  'webp',
  'awebp',
  'avif',
  'bmp',
  'svg',
]

const props = defineProps<{
  show: boolean
  theme: ThemePreset | { popup: string; fontColor: string }
  mode: 'chapter' | 'para'
  bookUrl: string
  chapterUrl: string
  bookSourceUrl?: string
  paraIndex?: number
  paraText?: string
  /** 章评第一页已由阅读页预取，打开面板时直接复用，避免重复请求 */
  initialPage?: ReviewPage | null
}>()

defineEmits<{ close: [] }>()

const items = ref<ReviewItem[]>([])
const total = ref(0)
const page = ref(1)
const hasMore = ref(false)
const loading = ref(false)
const error = ref('')
const listRef = ref<HTMLElement>()
const preview = ref('')

/** 每次打开都重新装载：评论会变，缓存里的旧数据不该拦住刷新 */
watch(
  () => [props.show, props.mode, props.paraIndex, props.chapterUrl] as const,
  () => {
    if (!props.show) return
    void reload()
  },
  { immediate: true },
)

async function reload() {
  error.value = ''
  const initial = props.mode === 'chapter' ? props.initialPage : null
  if (initial && initial.items?.length) {
    items.value = initial.items
    total.value = initial.total
    page.value = initial.page || 1
    hasMore.value = initial.hasMore
    return
  }
  items.value = []
  total.value = 0
  page.value = 1
  hasMore.value = false
  await fetchPage(1)
}

async function fetchPage(target: number) {
  loading.value = true
  error.value = ''
  try {
    const params = {
      bookUrl: props.bookUrl,
      chapterUrl: props.chapterUrl,
      bookSourceUrl: props.bookSourceUrl,
      page: target,
    }
    const resp =
      props.mode === 'para'
        ? await getParaComments({ ...params, paraIndex: props.paraIndex ?? 0 })
        : await getChapterComments(params)
    const data = resp.data
    const incoming = (data.items || []).map(normalizeItem)
    items.value = target <= 1 ? incoming : [...items.value, ...incoming]
    total.value = data.total
    page.value = target
    hasMore.value = data.hasMore
  } catch (err) {
    error.value = (err as Error)?.message || '评论加载失败'
  } finally {
    loading.value = false
  }
}

async function loadMore() {
  if (loading.value) return
  await fetchPage(page.value + 1)
  await nextTick()
  listRef.value?.scrollTo({ top: listRef.value.scrollHeight, behavior: 'smooth' })
}

/**
 * 一条评论的配图。
 *
 * 站点常为同一张图给出多个格式变体（番茄同时给 HEIC 和 JPEG），
 * 这里挑浏览器能直接渲染的第一个；都不认识时退回第一个，交给 `onerror` 兜底。
 */
function normalizeItem(item: ReviewItem): ReviewItem {
  const urls = item.images || []
  if (!urls.length) return { ...item, images: [] }
  const renderable = urls.find((url) => RENDERABLE_IMAGE_EXTENSIONS.includes(extensionOf(url)))
  return { ...item, images: [renderable || urls[0]] }
}

function extensionOf(url: string) {
  const path = url.split('?')[0].split('#')[0]
  const dot = path.lastIndexOf('.')
  return dot >= 0 ? path.slice(dot + 1).toLowerCase() : ''
}

/**
 * 评论时间。站点给的格式五花八门，这里只处理 Unix 秒级时间戳
 * （番茄系接口的形态），其余原样展示。
 */
function formatReviewTime(raw: string) {
  const text = (raw || '').trim()
  if (!/^\d{9,13}$/.test(text)) return text
  const ms = text.length <= 10 ? Number(text) * 1000 : Number(text)
  const date = new Date(ms)
  if (Number.isNaN(date.getTime())) return text
  const diffMinutes = Math.floor((Date.now() - date.getTime()) / 60000)
  if (diffMinutes < 1) return '刚刚'
  if (diffMinutes < 60) return `${diffMinutes} 分钟前`
  if (diffMinutes < 60 * 24) return `${Math.floor(diffMinutes / 60)} 小时前`
  if (diffMinutes < 60 * 24 * 7) return `${Math.floor(diffMinutes / 1440)} 天前`
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
}
</script>

<style scoped>
.comment-mask {
  position: fixed;
  inset: 0;
  z-index: 3000;
  display: flex;
  align-items: flex-end;
  justify-content: center;
  background: rgba(0, 0, 0, 0.35);
}

.comment-panel {
  display: flex;
  flex-direction: column;
  width: min(560px, 100%);
  max-height: min(78vh, 720px);
  padding-bottom: var(--safe-area-bottom);
  border-radius: 18px 18px 0 0;
  box-shadow: 0 -8px 32px rgba(0, 0, 0, 0.22);
}

/* 桌面端：底部抽屉在宽屏上离正文太远，改成右侧栏 */
@media (min-width: 768px) {
  .comment-mask {
    align-items: stretch;
    justify-content: flex-end;
    background: rgba(0, 0, 0, 0.28);
  }

  .comment-panel {
    width: min(400px, 42vw);
    max-height: none;
    height: 100%;
    padding-bottom: 0;
    border-radius: 16px 0 0 16px;
    box-shadow: -8px 0 32px rgba(0, 0, 0, 0.22);
  }
}

.comment-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 16px 10px;
  border-bottom: 1px solid rgba(128, 128, 128, 0.16);
}

.comment-title {
  font-size: 15px;
  font-weight: 600;
}

.comment-total {
  margin-left: 4px;
  color: var(--color-text-tertiary);
  font-weight: 400;
}

.comment-close {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 30px;
  height: 30px;
  padding: 0;
  color: inherit;
  background: transparent;
  border: none;
  border-radius: 50%;
  opacity: 0.7;
  cursor: pointer;
}

.comment-close svg {
  width: 16px;
  height: 16px;
}

.comment-quote {
  margin: 10px 16px 0;
  padding: 8px 12px;
  border-left: 3px solid var(--color-primary);
  background: rgba(128, 128, 128, 0.08);
  border-radius: 6px;
  color: var(--color-text-secondary);
  font-size: 13px;
  line-height: 1.6;
}

.comment-list {
  flex: 1;
  overflow-y: auto;
  padding: 8px 16px 16px;
  -webkit-overflow-scrolling: touch;
}

.comment-hint {
  padding: 28px 0;
  text-align: center;
  color: var(--color-text-tertiary);
  font-size: 13px;
}

.comment-error {
  color: var(--color-danger);
}

.comment-item {
  padding: 12px 0;
  border-bottom: 1px solid rgba(128, 128, 128, 0.1);
}

.comment-meta {
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.comment-name {
  font-size: 13px;
  font-weight: 600;
  opacity: 0.85;
}

.comment-time {
  font-size: 11px;
  color: var(--color-text-tertiary);
}

.comment-text {
  margin: 5px 0 0;
  font-size: 14px;
  line-height: 1.65;
  word-break: break-word;
  white-space: pre-wrap;
}

.comment-images {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 8px;
}

.comment-images img {
  max-width: 120px;
  max-height: 120px;
  border-radius: 8px;
  object-fit: cover;
  cursor: zoom-in;
}

.comment-preview {
  position: fixed;
  inset: 0;
  z-index: 3100;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  background: rgba(0, 0, 0, 0.88);
  cursor: zoom-out;
}

.comment-preview img {
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
}

.comment-replies {
  margin-top: 6px;
  padding: 6px 10px;
  background: rgba(128, 128, 128, 0.08);
  border-radius: 8px;
}

.comment-reply {
  margin: 0;
  font-size: 13px;
  line-height: 1.6;
  word-break: break-word;
}

.comment-reply + .comment-reply {
  margin-top: 4px;
}

.comment-reply-name {
  color: var(--color-primary);
}

.comment-reply-to {
  margin-left: 4px;
  color: var(--color-text-tertiary);
}

.comment-reply-more {
  margin-top: 4px;
  font-size: 12px;
  color: var(--color-text-tertiary);
}

.comment-actions {
  display: flex;
  align-items: center;
  margin-top: 6px;
  color: var(--color-text-tertiary);
  font-size: 12px;
}

.comment-digg {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.comment-digg svg {
  width: 13px;
  height: 13px;
}

.comment-more {
  display: block;
  width: 100%;
  margin-top: 12px;
  padding: 10px;
  color: var(--color-primary);
  background: transparent;
  border: 1px solid var(--color-primary-border);
  border-radius: 10px;
  font-size: 13px;
  cursor: pointer;
}

.comment-more:disabled {
  opacity: 0.6;
  cursor: default;
}

.comment-panel-enter-active,
.comment-panel-leave-active {
  transition: opacity 0.2s ease;
}

.comment-panel-enter-active .comment-panel,
.comment-panel-leave-active .comment-panel {
  transition: transform 0.24s ease;
}

.comment-panel-enter-from,
.comment-panel-leave-to {
  opacity: 0;
}

.comment-panel-enter-from .comment-panel,
.comment-panel-leave-to .comment-panel {
  transform: translateY(100%);
}

@media (min-width: 768px) {
  .comment-panel-enter-from .comment-panel,
  .comment-panel-leave-to .comment-panel {
    transform: translateX(100%);
  }
}
</style>
