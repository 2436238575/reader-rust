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
                class="ghost-btn"
                :disabled="resettingBook"
                title="所有章节改回显示最新上游正文（本地书回到原文）；历史版本保留在正文历史里"
                @click="handleResetBookToUpstream"
              >
                整书换回上游
              </button>
              <button class="ghost-btn" :disabled="loading" @click="reload">刷新</button>
              <button class="close-btn" @click="close" aria-label="关闭">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                  <path d="M18 6 6 18M6 6l12 12" />
                </svg>
              </button>
            </div>
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
                  <span class="source-tag"
                    >[{{ versionTagLabel(item) }}] {{ versionTagState(item) }}</span
                  >
                  <span v-if="item.isCurrent" class="badge current">当前</span>
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
                      <button
                        v-if="row.kind === 'collapse'"
                        class="diff-collapse"
                        @click="expandCollapse(row.opsStart)"
                      >
                        ⋯ 展开 15 段（还有 {{ row.opsCount }} 段未显示）
                      </button>
                      <div v-else-if="row.op.type === 'equal'" class="diff-row equal">
                        <span class="diff-gutter"></span>
                        <span class="diff-text">{{ row.op.editedText }}</span>
                      </div>
                      <div v-else-if="row.op.type === 'del'" class="diff-row del">
                        <span class="diff-gutter">-</span>
                        <span class="diff-text">{{ row.op.baseText }}</span>
                      </div>
                      <div v-else-if="row.op.type === 'add'" class="diff-row add">
                        <span class="diff-gutter">+</span>
                        <span class="diff-text">{{ row.op.editedText }}</span>
                      </div>
                      <template v-else>
                        <div class="diff-row del">
                          <span class="diff-gutter">-</span>
                          <span class="diff-text">{{ row.op.baseText }}</span>
                        </div>
                        <div class="diff-row add">
                          <span class="diff-gutter">+</span>
                          <span class="diff-text">
                            <template
                              v-for="(part, partIndex) in inlineParts(row.op)"
                              :key="partIndex"
                            >
                              <span v-if="part.added" class="diff-ins-strong">{{ part.text }}</span>
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
import { formatDateTime, formatShortDate } from '../../utils/format'
import {
  buildDiffRows,
  diffStats,
  inlineDiff,
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
const expanded = ref(new Map<number, number>())
const diffRows = computed(() =>
  buildDiffRows(diffOps.value ?? [], { expanded: expanded.value })
)

const showImport = ref(false)

const chapterTitle = computed(() => store.currentChapter?.title || '')
const localBook = computed(() => isLocalBook(store.book))
const canFollowLatest = computed(
  () => Boolean(timeline.value?.selectedVersionId) || store.config.contentVersion === 'upstream'
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

/** 版本小标签的来源名：上游用书源名（本地书用「本地」），用户版本用备注兜底类别名 */
function versionTagLabel(item: ChapterTimelineItem) {
  if (item.kind === 'upstream') return item.bookSourceName || '本地'
  return item.note || (item.kind === 'imported' ? '历史' : '编辑')
}

/** 标签状态：最新上游标「最新」，其余标短日期 */
function versionTagState(item: ChapterTimelineItem) {
  if (item.kind === 'upstream' && item.isLatestUpstream) return '最新'
  return formatShortDate(item.createdAt)
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
    await store.setChapterVersion(store.currentIndex, preview.value.versionId)
    appStore.showToast('已切换当前版本', 'success')
    await reload()
  } catch (e) {
    appStore.showToast((e as Error).message || '切换版本失败', 'error')
  }
}

async function followLatest() {
  try {
    await store.setChapterVersion(store.currentIndex, null)
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
      fromVersionId: preview.value.versionId,
      toVersionId: target,
    })
    diffOps.value = result.ops
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
    await store.invalidateChapterContentCache()
    await store.reloadChapterAfterEdit(store.currentIndex)
    await reload()
  } catch (e) {
    appStore.showToast((e as Error).message || '操作失败', 'error')
  } finally {
    resettingBook.value = false
  }
}

function expandCollapse(opsStart: number) {
  const next = new Map(expanded.value)
  next.set(opsStart, (next.get(opsStart) ?? 0) + 15)
  expanded.value = next
}

function inlineParts(op: ParagraphDiffOp) {
  const diff = inlineDiff(op.baseText || '', op.editedText || '')
  const parts: { text: string; added: boolean }[] = []
  if (diff.prefix) parts.push({ text: diff.prefix, added: false })
  if (diff.added) parts.push({ text: diff.added, added: true })
  if (diff.suffix) parts.push({ text: diff.suffix, added: false })
  return parts
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
  await store.invalidateChapterContentCache()
  await reload()
  if (makeCurrent && includesCurrent) {
    await store.reloadChapterAfterEdit(store.currentIndex)
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
  justify-content: space-between;
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

.source-tag {
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
  color: #ef4444;
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
  color: #ef4444;
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

.diff-collapse {
  display: block;
  width: 100%;
  margin: 4px 0;
  padding: 6px 0;
  border: 1px dashed rgba(128, 128, 128, 0.35);
  border-radius: 8px;
  background: transparent;
  color: inherit;
  opacity: 0.65;
  font-size: var(--text-xs);
  cursor: pointer;
  text-align: center;
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
    grid-template-rows: minmax(120px, 40%) minmax(0, 1fr);
  }
  .version-list {
    max-height: 40vh;
  }
  .head-actions {
    gap: 6px;
  }
}
</style>
