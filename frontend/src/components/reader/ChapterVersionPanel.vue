<template>
  <Teleport to="body">
    <Transition name="fade">
      <div v-if="modelValue" class="modal-overlay" @click="close"></div>
    </Transition>
    <Transition :name="isMobileLayout ? 'slide-right' : 'scale'">
      <div v-if="modelValue" class="modal-container" @click.self="close">
        <div class="version-modal">
          <div class="modal-head">
            <div class="head-main">
              <h2>正文历史</h2>
              <p class="head-sub">
                {{ chapterTitle }}
                <template v-if="timeline">
                  · {{ timeline.items.length }} 个版本
                </template>
              </p>
            </div>
            <div class="head-actions">
              <button v-if="!showImport" class="ghost-btn" @click="editCurrent">
                编辑当前
              </button>
              <button v-if="!showImport" class="ghost-btn" @click="showImport = true">
                导入
              </button>
              <button v-if="canFollowLatest" class="ghost-btn" @click="followLatest">
                跟随最新
              </button>
              <button
                class="ghost-btn danger"
                :disabled="resettingBook"
                title="所有章节改回显示最新上游正文（本地书回到原文）；历史版本保留在正文历史里"
                @click="handleResetBookToUpstream"
              >
                整书换回上游
              </button>
              <button class="ghost-btn" :disabled="loading" @click="reload">刷新</button>
            </div>
            <!-- 关闭按钮独立于操作区：移动端操作区横向滚动时不会被裁剪，始终钉在头部右上角 -->
            <button class="close-btn" @click="close" aria-label="关闭">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
            </button>
          </div>

          <div v-if="loading" class="empty">加载中…</div>
          <div v-else-if="error" class="empty">{{ error }}</div>
          <div v-else class="body">
            <div class="version-list">
              <button
                v-for="item in timeline?.items || []"
                :key="item.versionId"
                class="version-row"
                :class="{ active: preview?.versionId === item.versionId }"
                @click="previewVersion(item)"
              >
                <div class="row-main">
                  <span class="kind" :class="item.kind">{{ kindLabel(item.kind) }}</span>
                  <span class="time">{{ formatDateTime(item.createdAt) }}</span>
                  <span class="chars">{{ item.chars }} 字</span>
                </div>
                <div class="row-sub">
                  <span v-if="item.bookSourceName" class="source">{{ item.bookSourceName }}</span>
                  <span v-if="item.note" class="note">{{ item.note }}</span>
                  <span v-if="item.isCurrent" class="badge current">当前</span>
                  <span v-if="item.isLatestUpstream" class="badge">最新上游</span>
                  <span v-if="item.isLatestUser" class="badge">最新编辑</span>
                  <span v-if="item.rawSize === 0" class="badge empty">空正文</span>
                </div>
              </button>
              <div v-if="!timeline?.items?.length" class="empty small">还没有正文版本</div>
            </div>

            <div v-if="showImport" class="preview import-pane">
              <ChapterTextImport
                :book-id="store.book?.bookId"
                :chapter-url="store.currentChapter?.url"
                :chapter-index="store.currentChapter?.index"
                :chapter-title="store.currentChapter?.title"
                :local-book="localBook"
                @imported="onImported"
                @cancel="showImport = false"
                @cache="onOpenCache"
              />
            </div>

            <div v-else class="preview">
              <div v-if="!preview" class="empty small">选择一个版本预览</div>
              <template v-else>
                <div class="preview-head">
                  <div class="preview-meta">
                    <span>{{ kindLabel(preview.kind) }}</span>
                    <span>{{ formatDateTime(preview.createdAt) }}</span>
                    <span>{{ preview.chars }} 字</span>
                  </div>
                  <div class="preview-actions">
                    <button class="ghost-btn" @click="editPreview">编辑</button>
                    <button
                      class="ghost-btn"
                      :disabled="diffLoading || compareTargetId == null"
                      :title="compareTitle"
                      @click="comparePreview"
                    >
                      {{ diffLoading ? '对比中…' : '对比' }}
                    </button>
                    <button
                      class="ghost-btn"
                      :disabled="isPreviewCurrent"
                      @click="makeCurrent"
                    >
                      设为当前
                    </button>
                  </div>
                </div>

                <div v-if="diffError" class="empty small">{{ diffError }}</div>
                <div v-else-if="diffOps" class="diff-area">
                  <div class="diff-stats">
                    <span class="stat-add">+{{ diffStatsData?.added || 0 }} 段</span>
                    <span class="stat-del">-{{ diffStatsData?.removed || 0 }} 段</span>
                    <span class="stat-mod">~{{ diffStatsData?.modified || 0 }} 段</span>
                  </div>
                  <div class="diff-body">
                    <template v-for="(row, rowIndex) in diffRows" :key="rowIndex">
                      <div v-if="row.kind === 'collapse'" class="diff-collapse">
                        <button
                          class="diff-collapse-edge"
                          type="button"
                          aria-label="展开上方 3 段"
                          @click="expandRow(row, 'head')"
                        >
                          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                            <path d="m18 15-6-6-6 6" />
                          </svg>
                        </button>
                        <span class="diff-collapse-label">⋯ 还有 {{ row.opsCount }} 段未显示 ⋯</span>
                        <button
                          class="diff-collapse-edge"
                          type="button"
                          aria-label="展开下方 3 段"
                          @click="expandRow(row, 'tail')"
                        >
                          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                            <path d="m6 9 6 6 6-6" />
                          </svg>
                        </button>
                      </div>
                      <div v-else-if="row.op.type === 'equal'" class="diff-row equal">
                        <span class="diff-gutter"></span>
                        <span class="diff-text">{{ row.op.editedText }}</span>
                      </div>
                      <div v-else-if="row.op.type === 'del'" class="diff-row del">
                        <span class="diff-gutter">-</span>
                        <span class="diff-text">
                          <template
                            v-for="(part, partIndex) in inlinePartsFor('del', row.op)"
                            :key="partIndex"
                          >
                            <span v-if="part.changed" class="diff-del-strong">{{
                              part.text
                            }}</span>
                            <span v-else>{{ part.text }}</span>
                          </template>
                        </span>
                      </div>
                      <div v-else-if="row.op.type === 'add'" class="diff-row add">
                        <span class="diff-gutter">+</span>
                        <span class="diff-text">
                          <template
                            v-for="(part, partIndex) in inlinePartsFor('add', row.op)"
                            :key="partIndex"
                          >
                            <span v-if="part.changed" class="diff-ins-strong">{{
                              part.text
                            }}</span>
                            <span v-else>{{ part.text }}</span>
                          </template>
                        </span>
                      </div>
                      <template v-else>
                        <div class="diff-row del">
                          <span class="diff-gutter">-</span>
                          <span class="diff-text">
                            <template
                              v-for="(part, partIndex) in inlinePartsFor('del', row.op)"
                              :key="partIndex"
                            >
                              <span v-if="part.changed" class="diff-del-strong">{{
                                part.text
                              }}</span>
                              <span v-else>{{ part.text }}</span>
                            </template>
                          </span>
                        </div>
                        <div class="diff-row add">
                          <span class="diff-gutter">+</span>
                          <span class="diff-text">
                            <template
                              v-for="(part, partIndex) in inlinePartsFor('add', row.op)"
                              :key="partIndex"
                            >
                              <span v-if="part.changed" class="diff-ins-strong">{{
                                part.text
                              }}</span>
                              <span v-else>{{ part.text }}</span>
                            </template>
                          </span>
                        </div>
                      </template>
                    </template>
                  </div>
                </div>
                <div v-else class="preview-content">{{ preview.content }}</div>
              </template>
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, ref, toRef, watch } from 'vue'
import { useReaderStore } from '../../stores/reader'
import { useReaderContentStore } from '../../stores/readerContent'
import { useReaderConfigStore } from '../../stores/readerConfig'
import { useAppStore } from '../../stores/app'
import { useEscClose } from '../../composables/useEscClose'
import { useMobileLayout } from '../../composables/useMobileLayout'
import {
  getChapterTimeline,
  getChapterVersion,
  getChapterVersionDiff,
  resetBookToUpstream,
  type ChapterImportResult,
  type ChapterTimeline,
  type ChapterTimelineItem,
  type ChapterVersionDetail,
} from '../../api/chapterEdit'
import ChapterTextImport from './ChapterTextImport.vue'
import { isLocalBook } from '../../utils/localBook'
import { formatDateTime } from '../../utils/format'
import {
  buildDiffRows,
  diffStats,
  expandCollapseEdge,
  hydrateDiffOps,
  inlinePartsFor,
  type CollapseExpansion,
  type DiffRow,
  type ParagraphDiffOp,
} from '../../utils/chapterDiff'

const props = defineProps<{ modelValue: boolean }>()
const emit = defineEmits<{
  'update:modelValue': [value: boolean]
  /**
   * 「编辑」：null 表示编辑当前显示内容（章节还没有版本时的唯一入口）；
   * 带 seed 表示以预览版本为底稿。
   */
  edit: [seed: { content: string; versionId?: number } | null]
}>()

const store = useReaderStore()
const contentStore = useReaderContentStore()
const configStore = useReaderConfigStore()
const appStore = useAppStore()
const { isMobileLayout } = useMobileLayout()
useEscClose(toRef(props, 'modelValue'), () => emit('update:modelValue', false))

const loading = ref(false)
const error = ref('')
const timeline = ref<ChapterTimeline | null>(null)
const preview = ref<ChapterVersionDetail | null>(null)

const diffOps = ref<ParagraphDiffOp[] | null>(null)
const diffError = ref('')
const diffLoading = ref(false)
const diffStatsData = computed(() => (diffOps.value?.length ? diffStats(diffOps.value) : null))
// 折叠块展开进度：key = 折叠块原始起点（不随展开位移），value = 块顶/块底已多显示的段数
const expanded = ref(new Map<number, CollapseExpansion>())
const diffRows = computed(() =>
  buildDiffRows(diffOps.value ?? [], { expanded: expanded.value })
)

const showImport = ref(false)

const chapterTitle = computed(() => store.currentChapter?.title || '')
const localBook = computed(() => isLocalBook(store.book))
const canFollowLatest = computed(
  () => Boolean(timeline.value?.selectedVersionId) || configStore.config.contentVersion === 'upstream'
)
const isPreviewCurrent = computed(
  () => preview.value != null && preview.value.versionId === timeline.value?.effectiveVersionId
)

/**
 * 「对比」的基准：最新上游版本；预览的就是最新上游时没有可比对象。
 * 本地书没有上游版本行，退回与当前显示版本对比。
 */
const compareTargetId = computed(() => {
  if (!preview.value) return null
  const upstream = timeline.value?.latestUpstreamVersionId
  if (upstream != null) {
    return preview.value.versionId === upstream ? null : upstream
  }
  const fallback = timeline.value?.effectiveVersionId ?? timeline.value?.latestUserVersionId
  if (fallback == null || fallback === preview.value.versionId) return null
  return fallback
})
const compareTitle = computed(() => {
  if (!preview.value) return ''
  if (compareTargetId.value == null) {
    return preview.value.versionId === timeline.value?.latestUpstreamVersionId
      ? '这就是最新上游版本，没有可对比的差异'
      : '没有可对比的版本'
  }
  return timeline.value?.latestUpstreamVersionId != null
    ? '与最新上游版本的差异'
    : '与当前显示版本的差异'
})

function kindLabel(kind: string) {
  if (kind === 'edited') return '编辑'
  if (kind === 'imported') return '导入'
  return '上游'
}

function close() {
  emit('update:modelValue', false)
}

async function reload() {
  const bookId = store.book?.bookId
  const chapter = store.currentChapter
  if (!bookId || !chapter) return
  loading.value = true
  error.value = ''
  try {
    timeline.value = await getChapterTimeline({ bookId, chapterUrl: chapter.url })
    if (preview.value) {
      const stillThere = timeline.value?.items?.some(
        (item) => item.versionId === preview.value?.versionId
      )
      if (!stillThere) preview.value = null
    }
  } catch (e) {
    error.value = (e as Error).message || '加载正文历史失败'
  } finally {
    loading.value = false
  }
}

async function previewVersion(item: ChapterTimelineItem) {
  const bookId = store.book?.bookId
  const chapter = store.currentChapter
  if (!bookId || !chapter) return
  showImport.value = false
  resetDiff()
  try {
    preview.value = await getChapterVersion(bookId, chapter.url, item.versionId)
  } catch (e) {
    appStore.showToast((e as Error).message || '读取版本失败', 'error')
  }
}

function resetDiff() {
  diffOps.value = null
  diffError.value = ''
  expanded.value = new Map()
}

async function makeCurrent() {
  if (!preview.value) return
  try {
    await contentStore.setChapterVersion(store.currentIndex, preview.value.versionId)
    appStore.showToast('已切换当前版本', 'success')
    await reload()
  } catch (e) {
    appStore.showToast((e as Error).message || '切换版本失败', 'error')
  }
}

async function followLatest() {
  try {
    await contentStore.setChapterVersion(store.currentIndex, null)
    appStore.showToast('已跟随最新版本', 'success')
    await reload()
  } catch (e) {
    appStore.showToast((e as Error).message || '切换版本失败', 'error')
  }
}

async function comparePreview() {
  const bookId = store.book?.bookId
  const chapter = store.currentChapter
  const target = compareTargetId.value
  if (!bookId || !chapter || !preview.value || target == null) return
  diffLoading.value = true
  resetDiff()
  try {
    const result = await getChapterVersionDiff({
      bookId,
      chapterUrl: chapter.url,
      // diff 里 from 是基准、to 是新状态：基准是被对比的那份（最新上游或当前
      // 显示版本），预览的版本站在新状态一侧；传反会让 + / - 正好标反。
      fromVersionId: target,
      toVersionId: preview.value.versionId,
    })
    // 后端为省流量不给 equal 段带文本：用预览版本正文补回，上下文行才显示得出来
    diffOps.value = hydrateDiffOps(result.ops, preview.value.content)
  } catch (e) {
    diffError.value = (e as Error).message || '对比失败'
  } finally {
    diffLoading.value = false
  }
}

/** 「编辑」预览版本：正文预填进编辑器，基准版本指向它（保存后 diff 相对它计算） */
function editPreview() {
  if (!preview.value) return
  emit('edit', { content: preview.value.content, versionId: preview.value.versionId })
}

/** 「编辑当前」：不传底稿，编辑器走默认预填（当前显示内容；上游原版下会取回编辑版） */
function editCurrent() {
  emit('edit', null)
}

/** 整书换回最新上游：清掉全书版本选择与用户版本指针，历史版本保留 */
const resettingBook = ref(false)
async function handleResetBookToUpstream() {
  const bookId = store.book?.bookId
  if (!bookId || resettingBook.value) return
  if (
    !confirm(
      '整书换回最新上游？\n所有章节将显示最新上游正文（本地书回到原文）；编辑与导入的历史版本仍保留在正文历史里。'
    )
  )
    return
  resettingBook.value = true
  try {
    const { cleared } = await resetBookToUpstream(bookId)
    appStore.showToast(
      cleared > 0 ? `已整书换回最新上游（${cleared} 章）` : '已整书换回最新上游',
      'success'
    )
    // 全书显示都变了：预加载过的章节缓存全部作废，重载当前章与本面板
    await contentStore.invalidateChapterContentCache()
    await contentStore.reloadChapterAfterEdit(store.currentIndex)
    await reload()
  } catch (e) {
    appStore.showToast((e as Error).message || '操作失败', 'error')
  } finally {
    resettingBook.value = false
  }
}

/** VSCode 式逐次展开：每次从块顶或块底多显示 3 段，剩余保持折叠 */
function expandRow(row: Extract<DiffRow, { kind: 'collapse' }>, edge: 'head' | 'tail') {
  expanded.value = expandCollapseEdge(expanded.value, row, edge)
}

function onOpenCache() {
  emit('update:modelValue', false)
  store.openPanel('cache')
}

async function onImported(payload: {
  result: ChapterImportResult
  makeCurrent: boolean
  includesCurrent: boolean
}) {
  showImport.value = false
  const { result, makeCurrent, includesCurrent } = payload
  if (result.failed > 0) {
    const firstError = result.errors[0] ? `：${result.errors[0]}` : ''
    appStore.showToast(
      `导入完成：成功 ${result.imported} 章，失败 ${result.failed} 章${firstError}`,
      'warning'
    )
  } else if (result.imported > 0) {
    const unchanged = result.unchanged > 0 ? `，${result.unchanged} 章内容未变` : ''
    appStore.showToast(`已导入 ${result.imported} 章${unchanged}`, 'success')
  } else if (result.unchanged > 0) {
    appStore.showToast('内容和现有版本相同，没有新增版本', 'success')
  } else {
    appStore.showToast('没有导入任何章节', 'warning')
  }
  // 整本导入会一次改多章：预加载过的相邻章可能仍是旧正文，全部作废重取。
  await contentStore.invalidateChapterContentCache()
  await reload()
  if (makeCurrent && includesCurrent) {
    await contentStore.reloadChapterAfterEdit(store.currentIndex)
  }
}

watch(
  () => props.modelValue,
  (open) => {
    if (open) {
      preview.value = null
      resetDiff()
      void reload()
    } else {
      showImport.value = false
    }
  }
)

watch(
  () => store.currentIndex,
  () => {
    if (!props.modelValue) return
    preview.value = null
    resetDiff()
    void reload()
  }
)
</script>

<style scoped>
.modal-overlay {
  position: fixed;
  inset: 0;
  z-index: var(--z-modal);
  background: var(--overlay-mask-bg);
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

.version-modal {
  width: min(1100px, 100%);
  height: 86vh;
  display: flex;
  flex-direction: column;
  background: var(--color-bg-elevated);
  border-radius: 24px;
  padding: 22px;
  box-shadow: var(--shadow-xl);
}

.modal-head {
  display: flex;
  align-items: center;
  gap: 16px;
  margin-bottom: 14px;
  flex-shrink: 0;
}

.head-main h2 {
  margin: 0;
  font-size: var(--text-lg);
}

.head-sub {
  margin: 4px 0 0;
  font-size: var(--text-sm);
  opacity: 0.6;
}

.head-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  justify-content: flex-end;
  /* 关闭按钮移出后头部有 3 个 flex 子项：用 auto 外边距保持操作区右对齐（桌面端布局不变） */
  margin-left: auto;
}

.ghost-btn {
  padding: 6px 12px;
  border-radius: 8px;
  font-size: var(--text-sm);
  cursor: pointer;
  border: 1px solid var(--color-border, rgba(128, 128, 128, 0.25));
  background: transparent;
  color: inherit;
}

.ghost-btn:disabled {
  opacity: 0.45;
  cursor: default;
}

/* 破坏性操作保持 ghost 形态，仅用危险色提示（同 CacheLibraryModal 的 scope-btn.danger） */
.ghost-btn.danger {
  color: var(--color-danger);
  border-color: currentColor;
}

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
  flex-shrink: 0;
}

/* 桌面端：关闭按钮原是操作区最后一项（间距 8px），移出后抵消 modal-head 的 16px 间距保持一致 */
.modal-head > .close-btn {
  margin-left: -8px;
}

.close-btn:hover {
  opacity: 1;
  background: var(--color-bg-hover);
}

.close-btn svg {
  width: 18px;
  height: 18px;
}

.body {
  flex: 1;
  min-height: 0;
  display: grid;
  grid-template-columns: 300px minmax(0, 1fr);
  gap: 14px;
}

.version-list {
  overflow-y: auto;
  border: 1px solid rgba(128, 128, 128, 0.15);
  border-radius: 12px;
  padding: 6px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.version-row {
  width: 100%;
  text-align: left;
  border: 1px solid transparent;
  background: transparent;
  color: inherit;
  border-radius: 10px;
  padding: 8px 10px;
  cursor: pointer;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.version-row:hover {
  background: var(--color-bg-hover);
}

.version-row.active {
  border-color: var(--color-primary-border);
  background: var(--color-primary-bg);
}

.row-main {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: var(--text-sm);
}

.kind {
  font-size: var(--text-xs);
  padding: 1px 6px;
  border-radius: 999px;
  background: rgba(128, 128, 128, 0.16);
}

.kind.edited {
  background: color-mix(in srgb, var(--color-success) 16%, transparent);
  color: var(--color-success);
}

.kind.imported {
  background: color-mix(in srgb, var(--color-accent) 16%, transparent);
  color: var(--color-accent);
}

.time {
  opacity: 0.65;
  font-size: var(--text-xs);
}

.chars {
  margin-left: auto;
  opacity: 0.5;
  font-size: var(--text-xs);
}

.row-sub {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
  align-items: center;
  font-size: var(--text-xs);
  opacity: 0.7;
}

.source,
.note {
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.badge {
  padding: 0 6px;
  border-radius: 999px;
  background: rgba(128, 128, 128, 0.16);
}

.badge.current {
  background: var(--color-primary-bg);
  color: var(--color-primary);
}

.badge.empty {
  background: color-mix(in srgb, var(--color-warning) 18%, transparent);
  color: var(--color-warning);
}

.preview {
  min-width: 0;
  min-height: 0;
  display: flex;
  flex-direction: column;
  border: 1px solid rgba(128, 128, 128, 0.15);
  border-radius: 12px;
  overflow: hidden;
}

.preview-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 8px 12px;
  border-bottom: 1px solid rgba(128, 128, 128, 0.12);
  flex-wrap: wrap;
}

.preview-meta {
  display: flex;
  gap: 10px;
  font-size: var(--text-xs);
  opacity: 0.7;
}

.preview-actions {
  display: flex;
  gap: 8px;
}

.preview-content,
.diff-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
  padding: 10px 14px;
  font-size: var(--text-sm);
  line-height: 1.9;
  white-space: pre-wrap;
  word-break: break-word;
}

.diff-area {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.diff-stats {
  display: flex;
  gap: 10px;
  padding: 8px 12px;
  font-size: var(--text-xs);
}

.stat-add {
  color: #22c55e;
}

.stat-del {
  color: var(--color-danger);
}

.stat-mod {
  color: #3b82f6;
}

.diff-row {
  display: flex;
  gap: 10px;
  padding: 2px 0;
}

.diff-gutter {
  flex-shrink: 0;
  width: 18px;
  text-align: center;
  opacity: 0.6;
  user-select: none;
}

.diff-row.equal {
  opacity: 0.75;
}

.diff-row.del {
  background: rgba(239, 68, 68, 0.1);
}

.diff-row.del .diff-gutter {
  color: var(--color-danger);
}

.diff-row.add {
  background: rgba(34, 197, 94, 0.1);
}

.diff-row.add .diff-gutter {
  color: #22c55e;
}

.diff-ins-strong {
  background: rgba(34, 197, 94, 0.28);
  border-radius: 2px;
}

.diff-del-strong {
  background: rgba(239, 68, 68, 0.28);
  border-radius: 2px;
}

/* 折叠条：两端是逐次展开 3 段的上下箭头（VSCode 式），中间提示剩余段数 */
.diff-collapse {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 4px 0;
  padding: 2px 6px;
  border: 1px dashed rgba(128, 128, 128, 0.35);
  border-radius: 8px;
  opacity: 0.8;
  font-size: var(--text-xs);
}

.diff-collapse-label {
  flex: 1;
  text-align: center;
  opacity: 0.75;
}

.diff-collapse-edge {
  width: 28px;
  height: 26px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 6px;
  color: inherit;
  opacity: 0.65;
  flex-shrink: 0;
}

.diff-collapse-edge:hover {
  background: var(--color-bg-hover);
  opacity: 1;
}

.diff-collapse-edge svg {
  width: 14px;
  height: 14px;
}

.empty {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  opacity: 0.55;
  font-size: var(--text-sm);
}

.empty.small {
  padding: 20px;
}

.import-pane {
  padding: 10px 12px;
}

@media (max-width: 767px) {
  .modal-overlay {
    display: none;
  }
  .modal-container {
    padding: 0;
  }
  .version-modal {
    width: 100%;
    height: 100%;
    border-radius: 0;
    padding: 16px;
    padding-top: calc(16px + var(--safe-area-top));
    padding-bottom: calc(16px + var(--safe-area-bottom));
  }
  .body {
    grid-template-columns: 1fr;
    /* 列表行按内容收缩（上限由 version-list 的 40vh 约束），剩余空间全给预览区 */
    grid-template-rows: auto minmax(0, 1fr);
  }
  .version-list {
    max-height: 40vh;
  }
  .modal-head {
    flex-wrap: wrap;
    position: relative;
    gap: 10px;
  }
  .head-main {
    /* 标题独占第一行，右侧给绝对定位的关闭按钮（32px + 间隙）让位 */
    flex: 1 1 100%;
    padding-right: 40px;
  }
  .head-actions {
    /* 操作按钮独占第二行，超出时横向滚动，不再挤压标题与关闭按钮 */
    flex: 1 1 100%;
    flex-wrap: nowrap;
    justify-content: flex-start;
    gap: 6px;
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
  }
  .head-actions .ghost-btn {
    flex-shrink: 0;
  }
  .modal-head > .close-btn {
    /* 关闭按钮是 modal-head 的直接子元素，与横向滚动的操作区彻底解耦 */
    position: absolute;
    top: 0;
    right: 0;
  }
}
</style>
