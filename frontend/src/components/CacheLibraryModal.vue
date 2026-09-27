<template>
  <Teleport to="body">
    <Transition name="fade">
      <div v-if="modelValue" class="modal-overlay" @click="close"></div>
    </Transition>
    <Transition :name="isMobileLayout ? 'slide-right' : 'scale'">
      <div v-if="modelValue" class="modal-container" @click.self="close">
        <div class="cache-modal">
          <div class="modal-head">
            <div>
              <h2>缓存管理</h2>
              <p>查看并清理所有书籍的服务端缓存与浏览器缓存</p>
            </div>
            <div class="head-actions">
              <button class="ghost-btn refresh-btn" @click="refreshData" aria-label="刷新" title="刷新">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                  <path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
                  <path d="M3 3v5h5" />
                  <path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16" />
                  <path d="M16 16h5v5" />
                </svg>
                <span>刷新</span>
              </button>
              <button class="close-btn" @click="close">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                  <path d="M18 6 6 18M6 6l12 12" />
                </svg>
              </button>
            </div>
          </div>

          <div v-if="loading" class="loading-state">
            <div class="loading-spinner"></div>
            <p>缓存信息加载中...</p>
          </div>

          <div v-else class="cache-list">
            <div class="cache-toolbar">
              <div class="cache-scope">
                <span class="scope-label">默认缓存范围</span>
                <select v-model="cacheCount" class="scope-select">
                  <option :value="50">50章</option>
                  <option :value="100">100章</option>
                  <option :value="0">全本</option>
                </select>
              </div>
              <div class="cache-overview">
                <span>可离线书籍 {{ offlineReadyCount }} 本</span>
                <span>浏览器缓存章节 {{ totalBrowserCachedCount }} 章</span>
              </div>
            </div>

            <div v-for="item in mergedBooks" :key="item.bookUrl" class="cache-item">
              <div class="cache-main">
                <h3>{{ item.name }}</h3>
                <p>{{ item.author || '未知作者' }}</p>
                <div class="cache-stats">
                  <span>服务端 {{ item.serverCachedCount }} 章</span>
                  <span>浏览器 {{ item.browserCachedCount }} 章</span>
                </div>
              </div>
              <div class="cache-actions">
                <button @click="cacheServer(item.book)">缓存到服务器</button>
                <button @click="cacheBrowser(item.book)">缓存到浏览器</button>
                <button @click="clearServer(item.book)">清服务端</button>
                <button @click="clearBrowser(item.book)">清浏览器</button>
              </div>
            </div>

            <div class="cache-layers">
              <button class="layers-toggle" type="button" @click="showCachePurge = !showCachePurge">
                <span class="scope-label">缓存清理</span>
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  :class="{ expanded: showCachePurge }"
                >
                  <path d="m6 9 6 6 6-6" />
                </svg>
              </button>
              <div v-if="showCachePurge" class="layers-body">
                <div class="layer-row">
                  <div class="layer-info">
                    <span class="layer-label">浏览器缓存</span>
                    <small>本机 IndexedDB 里的离线章节</small>
                  </div>
                  <span class="layer-usage">{{ browserCacheUsageText }}</span>
                  <button class="scope-btn" :disabled="purging !== ''" @click="purgeBrowser">
                    {{ purging === 'browser' ? '清理中' : '清空' }}
                  </button>
                </div>
                <div class="layers-head">
                  <div class="layer-info">
                    <span class="layer-label">服务端缓存</span>
                    <small>服务端缓存按类型清理，清理后需重新抓取</small>
                  </div>
                  <button class="scope-btn danger" :disabled="purging !== ''" @click="purgeAllLayers">
                    {{ purging === 'all' ? '清理中...' : '全部清理' }}
                  </button>
                </div>
                <div v-for="layer in cacheLayers" :key="layer.kind" class="layer-row">
                  <div class="layer-info">
                    <span class="layer-label">{{ layer.label }}</span>
                    <small>{{ layer.hint }}</small>
                  </div>
                  <span class="layer-usage">{{ layerUsage(layer.kind) }}</span>
                  <button class="scope-btn" :disabled="purging !== ''" @click="purgeLayer(layer)">
                    {{ purging === layer.kind ? '清理中' : '清理' }}
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useBookshelfStore } from '../stores/bookshelf'
import { useAppStore } from '../stores/app'
import { getBookshelfWithCacheInfo, deleteBookCache } from '../api/bookshelf'
import type { Book } from '../types'
import { clearAllBrowserCache, deleteBrowserBookCache, listBrowserCacheSummary } from '../utils/browserCache'
import { cacheBookToBrowser } from '../utils/bookCache'
import {
  cacheBookSSE,
  getCacheStats,
  purgeAllUserCache,
  purgeCacheByKind,
  type CacheStats,
  type ServerCacheKind,
} from '../api/cache'
import { isLocalBook } from '../utils/localBook'
import { formatBytes } from '../utils/format'
import { useMobileLayout } from '../composables/useMobileLayout'

const props = defineProps<{
  modelValue: boolean
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
}>()

const shelfStore = useBookshelfStore()
const appStore = useAppStore()
const loading = ref(false)
const cacheCount = ref(50)
const serverBooks = ref<Book[]>([])
const browserSummaries = ref<Array<{ bookUrl: string; cachedChapterCount: number }>>([])

interface CacheLayer {
  kind: ServerCacheKind
  label: string
  hint: string
}

const cacheLayers: CacheLayer[] = [
  { kind: 'content', label: '正文缓存', hint: '已抓取的章节正文' },
  { kind: 'cover', label: '封面与图片', hint: '书籍封面、章节配图与评论图片' },
  { kind: 'chapterList', label: '目录与详情', hint: '章节目录与书籍详情缓存' },
  { kind: 'searchResults', label: '搜索结果', hint: '搜索与书海的结果缓存' },
  { kind: 'review', label: '评论缓存', hint: '章评与段评' },
]

const serverStats = ref<CacheStats | null>(null)
const purging = ref('')
const showCachePurge = ref(false)

// 移动端整屏铺开（对齐导航菜单/设置抽屉的断点），入场动画随之换成侧滑
const { isMobileLayout } = useMobileLayout()

const browserCacheUsageText = computed(() => {
  const chapters = browserSummaries.value.reduce((sum, item) => sum + item.cachedChapterCount, 0)
  const books = browserSummaries.value.filter((item) => item.cachedChapterCount > 0).length
  return chapters > 0 ? `${chapters} 章 · ${books} 本书` : '空'
})

function layerUsage(kind: ServerCacheKind) {
  const usage = serverStats.value?.[kind]
  if (!usage) return '—'
  return `${usage.files} 个文件 · ${formatBytes(usage.bytes)}`
}

function purgedTotal(result: Record<string, number>) {
  return Object.values(result).reduce((sum, n) => sum + n, 0)
}

async function purgeBrowser() {
  if (!confirm('确定清空本机浏览器缓存吗？所有已缓存到浏览器的离线章节都会被删除。')) return
  purging.value = 'browser'
  try {
    await clearAllBrowserCache()
    appStore.showToast('浏览器缓存已清空', 'success')
    await refreshData()
  } catch (error) {
    appStore.showToast((error as Error).message || '清空失败', 'error')
  } finally {
    purging.value = ''
  }
}

async function purgeLayer(layer: CacheLayer) {
  if (!confirm(`确定清理全部书籍的${layer.label}吗？清理后相关内容需要重新抓取。`)) return
  purging.value = layer.kind
  try {
    const result = await purgeCacheByKind(layer.kind)
    appStore.showToast(`${layer.label}已清理，删除 ${purgedTotal(result.purged)} 个文件`, 'success')
    await refreshData()
  } catch (error) {
    appStore.showToast((error as Error).message || '清理失败', 'error')
  } finally {
    purging.value = ''
  }
}

async function purgeAllLayers() {
  if (!confirm('确定清理全部服务端缓存吗？正文、封面图片、目录、搜索结果与评论缓存都会被删除。')) return
  purging.value = 'all'
  try {
    const result = await purgeAllUserCache()
    appStore.showToast(`服务端缓存已全部清理，删除 ${purgedTotal(result.purged)} 个文件`, 'success')
    await refreshData()
  } catch (error) {
    appStore.showToast((error as Error).message || '清理失败', 'error')
  } finally {
    purging.value = ''
  }
}

const mergedBooks = computed(() => {
  const serverMap = new Map(serverBooks.value.map((book) => [book.bookUrl, book.cachedChapterCount || 0]))
  const browserMap = new Map(browserSummaries.value.map((item) => [item.bookUrl, item.cachedChapterCount]))

  return shelfStore.books
    .filter((book) => !isLocalBook(book))
    .map((book) => ({
      book,
      bookUrl: book.bookUrl,
      name: book.name,
      author: book.author,
      serverCachedCount: serverMap.get(book.bookUrl) || 0,
      browserCachedCount: browserMap.get(book.bookUrl) || 0,
    }))
})

const offlineReadyCount = computed(() => mergedBooks.value.filter((item) => item.browserCachedCount > 0).length)
const totalBrowserCachedCount = computed(() => mergedBooks.value.reduce((sum, item) => sum + item.browserCachedCount, 0))

watch(() => props.modelValue, (visible) => {
  if (visible) {
    refreshData()
  }
})

function close() {
  emit('update:modelValue', false)
}

async function awaitSafeBrowserSummary() {
  return listBrowserCacheSummary().catch(() => [])
}

async function refreshData() {
  loading.value = true
  try {
    const [server, browser, stats] = await Promise.all([
      getBookshelfWithCacheInfo().catch(() => []),
      awaitSafeBrowserSummary(),
      getCacheStats().catch(() => null),
    ])
    serverBooks.value = server
    browserSummaries.value = browser
    serverStats.value = stats
  } finally {
    loading.value = false
  }
}

function cacheServer(book: Book) {
  const sse = cacheBookSSE({ bookUrl: book.bookUrl, count: cacheCount.value, concurrentCount: 8 })
  sse.addEventListener('end', async () => {
    sse.close()
    appStore.showToast(`"${book.name}" 已缓存到服务器`, 'success')
    await refreshData()
  })
  sse.onerror = () => {
    sse.close()
    appStore.showToast(`"${book.name}" 服务端缓存失败`, 'error')
  }
}

async function cacheBrowser(book: Book) {
  try {
    await cacheBookToBrowser({ book, startIndex: 0, count: cacheCount.value || undefined })
    appStore.showToast(`"${book.name}" 已缓存到浏览器`, 'success')
    await refreshData()
  } catch (error) {
    appStore.showToast((error as Error).message || '浏览器缓存失败', 'error')
  }
}

async function clearServer(book: Book) {
  await deleteBookCache(book.bookUrl)
  appStore.showToast(`"${book.name}" 服务端缓存已清除`, 'success')
  await refreshData()
}

async function clearBrowser(book: Book) {
  await deleteBrowserBookCache(book.bookUrl)
  appStore.showToast(`"${book.name}" 浏览器缓存已清除`, 'success')
  await refreshData()
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
  padding: 24px;
}

.cache-modal {
  width: min(960px, 100%);
  max-height: 82vh;
  overflow: auto;
  background: var(--color-bg-elevated);
  border-radius: 24px;
  padding: 24px;
  box-shadow: var(--shadow-xl);
}

.modal-head {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 20px;
}

.modal-head h2 {
  margin: 0;
}

.modal-head p {
  margin: 6px 0 0;
  color: var(--color-text-tertiary);
}

.head-actions {
  display: flex;
  gap: 8px;
  align-items: center;
}

.ghost-btn,
.cache-actions button {
  border: 1px solid var(--color-border);
  background: transparent;
  border-radius: 12px;
  padding: 8px 12px;
  cursor: pointer;
}

/* 关闭按钮与其他弹窗统一：无边框纯图标，悬停出底色 */
.close-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 36px;
  height: 36px;
  padding: 0;
  border: none;
  background: transparent;
  border-radius: 12px;
  color: var(--color-text-secondary);
  cursor: pointer;
}

.close-btn:hover {
  background: var(--color-bg-hover);
  color: var(--color-text);
}

.refresh-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.refresh-btn svg {
  width: 15px;
  height: 15px;
}

.close-btn svg {
  width: 16px;
  height: 16px;
}

.loading-state {
  min-height: 240px;
  display: flex;
  flex-direction: column;
  justify-content: center;
  align-items: center;
  gap: 16px;
}

.loading-spinner {
  width: 32px;
  height: 32px;
  border: 3px solid var(--color-border);
  border-top-color: var(--color-primary);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}

.cache-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.cache-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 14px 16px;
  border: 1px solid var(--color-border-light);
  border-radius: 18px;
  background: var(--color-bg-sunken);
}

.cache-scope {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.scope-label {
  font-size: var(--text-sm);
  color: var(--color-text-secondary);
}

.scope-select {
  min-height: 32px;
  padding: 0 var(--space-3);
  border: 1px solid var(--color-border);
  border-radius: 999px;
  background: transparent;
  cursor: pointer;
}

.scope-btn {
  border: 1px solid var(--color-border);
  background: transparent;
  border-radius: 999px;
  padding: 6px 12px;
  cursor: pointer;
}

.cache-overview {
  display: flex;
  gap: 14px;
  flex-wrap: wrap;
  font-size: var(--text-sm);
  color: var(--color-text-secondary);
}

.cache-layers {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 14px 16px;
  border: 1px solid var(--color-border-light);
  border-radius: 18px;
  background: var(--color-bg-sunken);
}

.layers-toggle {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  width: 100%;
  padding: 4px 0;
  border: none;
  background: none;
  cursor: pointer;
  color: inherit;
  font: inherit;
}

.layers-toggle svg {
  width: 16px;
  height: 16px;
  color: var(--color-text-tertiary);
  transition: transform var(--duration-fast);
}

.layers-toggle svg.expanded {
  transform: rotate(180deg);
}

.layers-body {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.layers-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 8px;
}

.layers-hint {
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.scope-btn.danger {
  color: var(--color-danger);
  border-color: currentColor;
}

.layer-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 0;
  border-top: 1px solid var(--color-divider);
}

.layer-info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.layer-label {
  font-size: var(--text-sm);
  font-weight: 600;
}

.layer-info small {
  color: var(--color-text-tertiary);
}

.layer-usage {
  font-size: var(--text-xs);
  color: var(--color-text-secondary);
  white-space: nowrap;
}

.cache-item {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  border: 1px solid var(--color-border-light);
  border-radius: 18px;
  padding: 18px;
}

.cache-main h3 {
  margin: 0;
}

.cache-main p {
  margin: 6px 0 10px;
  color: var(--color-text-tertiary);
}

.cache-stats {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  font-size: var(--text-sm);
}

.cache-actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  justify-content: flex-end;
  align-content: flex-start;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

/* 移动端与设置抽屉一样整屏铺开；遮罩被完全盖住，直接隐藏 */
@media (max-width: 767px) {
  .modal-overlay {
    display: none;
  }

  .modal-container {
    padding: 0;
  }

  .cache-modal {
    width: 100%;
    height: 100%;
    max-height: none;
    border-radius: 0;
    padding: calc(24px + var(--safe-area-top)) 16px calc(24px + var(--safe-area-bottom));
  }

  .cache-toolbar {
    flex-direction: column;
    align-items: flex-start;
  }

  .layer-row {
    flex-wrap: wrap;
  }

  .cache-item {
    flex-direction: column;
  }

  .cache-actions {
    justify-content: flex-start;
  }

  /* 四个操作按钮排成整齐的 2×2，而不是按内容长度参差不齐地折行 */
  .cache-actions button {
    flex: 1 1 calc(50% - 4px);
    padding-left: 4px;
    padding-right: 4px;
    text-align: center;
    white-space: nowrap;
  }

  /* 头部空间让给标题，刷新收成与关闭按钮同尺寸的纯图标 */
  .refresh-btn span {
    display: none;
  }

  .refresh-btn {
    border: none;
    padding: 0;
    width: 36px;
    height: 36px;
    justify-content: center;
  }
}
</style>
