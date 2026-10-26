<template>
  <div class="import-panel">
    <div class="import-head">
      <div class="head-text">
        <h3>导入</h3>
        <p>整本会按章节标题拆分，自动与本书目录匹配；预览确认后再写入正文历史。</p>
      </div>
      <button class="ghost-btn" :disabled="importing" @click="$emit('cancel')">返回版本预览</button>
    </div>

    <div class="mode-tabs" role="tablist">
      <button
        :class="{ active: mode === 'book' }"
        role="tab"
        :aria-selected="mode === 'book'"
        @click="switchMode('book')"
      >
        整本 TXT
      </button>
      <button
        :class="{ active: mode === 'single' }"
        role="tab"
        :aria-selected="mode === 'single'"
        @click="switchMode('single')"
      >
        当前章节
      </button>
    </div>

    <div class="file-row">
      <input
        ref="fileInput"
        type="file"
        accept=".txt,text/plain"
        class="file-input"
        @change="onFileChange"
      />
      <button class="ghost-btn" @click="pickFile">选择 TXT 文件</button>
      <template v-if="file">
        <span class="file-name" :title="file.name">{{ file.name }}</span>
        <span class="file-size">{{ formatCount(inputChars) }}</span>
        <button class="text-btn" @click="clearFile">移除</button>
      </template>
      <span v-else class="hint">或粘贴到下方文本框</span>
    </div>

    <textarea
      v-if="!file"
      v-model="pastedText"
      class="text-input"
      :placeholder="mode === 'book' ? '粘贴整本小说文本，自动按「第 X 章」等标题拆分' : '粘贴这一章的正文（可以带标题行，导入时会去掉）'"
    ></textarea>

    <div class="option-row">
      <label class="option">
        <span>版本类型</span>
        <select v-model="kind">
          <option value="edited">编辑版</option>
          <option value="imported">历史导入</option>
        </select>
      </label>
      <label class="check">
        <input v-model="makeCurrent" type="checkbox" />
        导入后设为当前版本
      </label>
      <label v-if="kind === 'imported'" class="option">
        <span>版本时间</span>
        <input v-model="importAt" type="datetime-local" />
      </label>
      <input v-model="note" class="note-input" placeholder="备注（可选）" />
    </div>

    <div class="action-row">
      <span class="hint">
        支持 UTF-8 / GB18030 / UTF-16 编码；整本导入只写入勾选的已匹配章节。
      </span>
      <button class="primary-btn" :disabled="previewing || importing || !canPreview" @click="runPreview">
        {{ previewing ? '解析中…' : '解析并预览' }}
      </button>
    </div>

    <div v-if="error" class="error-text">{{ error }}</div>

    <div v-if="preview" class="preview-area">
      <div class="preview-summary">
        <span>共 {{ preview.total }} 章 · {{ previewChars.toLocaleString('zh-CN') }} 字</span>
        <span>匹配 {{ preview.matched }}</span>
        <span v-if="preview.unmatched">未匹配 {{ preview.unmatched }}</span>
        <span v-if="preview.empty">空章节 {{ preview.empty }}</span>
        <span class="summary-changed">有差异 {{ preview.changed }}</span>
        <span>一致 {{ preview.cached - preview.changed }}</span>
        <span v-if="preview.uncached" class="summary-uncached">未缓存 {{ preview.uncached }}</span>
        <span class="summary-selected">已选 {{ selectedCount }}</span>
      </div>

      <div v-if="preview.uncached > 0" class="cache-hint">
        <span v-if="localBook">
          有 {{ preview.uncached }} 章还没有可对比的正文版本；本地书导入后会建立版本，不需要缓存上游。
        </span>
        <span v-else>
          有 {{ preview.uncached }} 章还没有缓存正文，没法对比差异。建议先缓存全书再回来预览。
        </span>
        <button v-if="!localBook" class="text-btn" @click="$emit('cache')">去缓存全书</button>
      </div>
      <div v-if="selectedCount === 0 && preview.matched > 0" class="same-hint">
        默认只勾选有差异的章节；需要覆盖现有正文时可以手动勾选。
      </div>

      <div class="preview-toolbar">
        <div class="filter-tabs">
          <button :class="{ active: filter === 'all' }" @click="filter = 'all'">全部</button>
          <button :class="{ active: filter === 'changed' }" @click="filter = 'changed'">
            有差异（{{ preview.changed }}）
          </button>
          <button :class="{ active: filter === 'unmatched' }" @click="filter = 'unmatched'">
            未匹配 / 空（{{ preview.unmatched + preview.empty }}）
          </button>
        </div>
        <div class="selection-actions">
          <button class="text-btn" @click="selectChanged">全选有差异</button>
          <button class="text-btn" @click="selectAllMatched">全选已匹配</button>
          <button class="text-btn" @click="clearSelection">全不选</button>
        </div>
      </div>

      <div class="preview-body">
        <div class="chapter-list">
          <div
            v-for="item in visibleChapters"
            :key="item.sourceIndex"
            class="chapter-row"
            :class="{ active: active?.sourceIndex === item.sourceIndex, disabled: item.status !== 'matched' }"
            role="button"
            tabindex="0"
            @click="setActive(item)"
            @keydown.enter="setActive(item)"
          >
            <input
              type="checkbox"
              :checked="selected.has(item.sourceIndex)"
              :disabled="item.status !== 'matched'"
              @click.stop="toggle(item)"
            />
            <div class="chapter-main">
              <span class="chapter-title">{{ item.title || '（无标题）' }}</span>
              <span class="chapter-sub">
                <span class="match-badge" :class="item.matchKind">{{ matchLabel(item) }}</span>
                <template v-if="item.chapterTitle && item.chapterTitle !== item.title">
                  → {{ item.chapterTitle }}
                </template>
              </span>
            </div>
            <span class="chapter-side">
              <span class="status" :class="compareClass(item)">{{ compareLabel(item) }}</span>
              <span class="chars">{{ item.chars }} 字</span>
            </span>
          </div>

          <button v-if="canShowMore" class="show-more" @click="displayLimit += 500">
            显示更多（已显示 {{ visibleChapters.length }} / {{ filteredChapters.length }}）
          </button>
          <div v-if="!filteredChapters.length" class="empty small">没有符合筛选条件的章节</div>
        </div>

        <div class="chapter-detail">
          <template v-if="active">
            <div class="detail-title">{{ active.title || '（无标题）' }}</div>
            <div class="detail-meta">
              <span class="status" :class="compareClass(active)">{{ compareLabel(active) }}</span>
              <span v-if="active.chapterTitle">目录：{{ active.chapterTitle }}</span>
              <span>{{ active.chars }} 字</span>
              <span v-if="activeDiff?.data?.currentChars != null">
                当前 {{ activeDiff.data.currentChars }} 字
              </span>
            </div>

            <div v-if="active.status !== 'matched'" class="detail-note">
              {{ active.status === 'empty' ? '空章节不会写入。' : '没有匹配到目录，无法导入。' }}
            </div>
            <div v-else-if="!active.cached" class="detail-note">
              本章还没有缓存正文，无法和现有正文对比。
            </div>
            <div v-else-if="!active.changed" class="detail-note">
              与当前正文一致，默认不勾选。
            </div>

            <div v-if="diffLoading" class="empty small">正在对比…</div>
            <div v-else-if="diffOps?.length" class="detail-diff">
              <template v-for="(row, rowIndex) in activeDiffRows" :key="rowIndex">
                <button
                  v-if="row.kind === 'collapse'"
                  class="diff-collapse"
                  @click="expandDiff(row.opsStart)"
                >
                  ⋯ 展开 {{ row.opsCount }} 段
                </button>
                <div v-else-if="row.op.type === 'equal'" class="diff-line equal">
                  {{ row.op.editedText }}
                </div>
                <div v-else-if="row.op.type === 'del'" class="diff-line del">
                  <span class="diff-sign">-</span>{{ row.op.baseText }}
                </div>
                <div v-else-if="row.op.type === 'add'" class="diff-line add">
                  <span class="diff-sign">+</span>{{ row.op.editedText }}
                </div>
                <template v-else>
                  <div class="diff-line del">
                    <span class="diff-sign">-</span>{{ row.op.baseText }}
                  </div>
                  <div class="diff-line add">
                    <span class="diff-sign">+</span>{{ row.op.editedText }}
                  </div>
                </template>
              </template>
            </div>
            <div v-else-if="diffError" class="detail-note error">{{ diffError }}</div>

            <template v-if="showDetailExcerpt">
              <div v-if="diffError && activeDiff?.data?.currentExcerpt" class="detail-block">
                <div class="detail-label">当前正文</div>
                <pre class="detail-excerpt">{{ activeDiff.data.currentExcerpt }}</pre>
              </div>
              <div class="detail-block">
                <div class="detail-label">{{ diffError ? '导入正文' : '正文片段' }}</div>
                <pre class="detail-excerpt">{{ active.excerpt || '（空章节）' }}</pre>
              </div>
            </template>
          </template>
          <div v-else class="empty small">选择左侧章节查看差异</div>
        </div>
      </div>

      <div class="import-footer">
        <span class="hint">默认只勾选与当前正文有差异的章节；未匹配、空章节和未勾选的章节不会写入。</span>
        <button
          class="primary-btn"
          :disabled="importing || selectedCount === 0"
          @click="doImport"
        >
          {{ importing ? `导入中…（${selectedCount} 章）` : `导入 ${selectedCount} 章` }}
        </button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import {
  getChapterImportDiff,
  importChapterText,
  previewChapterImport,
  type ChapterImportDiff,
  type ChapterImportMode,
  type ChapterImportPreview,
  type ChapterImportPreviewChapter,
  type ChapterImportParams,
  type ChapterImportResult,
} from '../../api/chapterEdit'
import { buildDiffRows } from '../../utils/chapterDiff'

const props = defineProps<{
  bookId?: string
  chapterUrl?: string
  chapterIndex?: number
  chapterTitle?: string
  localBook?: boolean
}>()

const emit = defineEmits<{
  imported: [
    payload: {
      result: ChapterImportResult
      makeCurrent: boolean
      includesCurrent: boolean
    },
  ]
  cancel: []
  cache: []
}>()

const mode = ref<'book' | 'single'>('book')
const kind = ref<'edited' | 'imported'>('edited')
const makeCurrent = ref(true)
const note = ref('')
const importAt = ref('')
const file = ref<File | null>(null)
const fileChars = ref<number | null>(null)
const pastedText = ref('')
const fileInput = ref<HTMLInputElement | null>(null)

const preview = ref<ChapterImportPreview | null>(null)
const selected = ref<Set<number>>(new Set())
const active = ref<ChapterImportPreviewChapter | null>(null)
const filter = ref<'all' | 'changed' | 'unmatched'>('all')
const displayLimit = ref(500)
const previewing = ref(false)
const importing = ref(false)
const error = ref('')

interface DiffState {
  loading: boolean
  data: ChapterImportDiff | null
  error: string
}

const diffStates = ref(new Map<number, DiffState>())
const expandedDiff = ref(new Map<number, number>())

const canPreview = computed(() => {
  if (!props.bookId) return false
  if (!file.value && !pastedText.value.trim()) return false
  return mode.value !== 'single' || Boolean(props.chapterUrl)
})

const pastedChars = computed(() => countChars(pastedText.value))
const inputChars = computed(() => (file.value ? fileChars.value : pastedChars.value))
const previewChars = computed(() =>
  (preview.value?.chapters ?? []).reduce((sum, item) => sum + item.chars, 0)
)

const filteredChapters = computed(() => {
  const chapters = preview.value?.chapters ?? []
  if (filter.value === 'changed') {
    return chapters.filter((item) => item.status === 'matched' && item.changed)
  }
  if (filter.value === 'unmatched') {
    return chapters.filter((item) => item.status !== 'matched')
  }
  return chapters
})

const visibleChapters = computed(() => filteredChapters.value.slice(0, displayLimit.value))
const canShowMore = computed(() => filteredChapters.value.length > displayLimit.value)
const selectedCount = computed(() => {
  const set = selected.value
  return (preview.value?.chapters ?? []).filter((item) => set.has(item.sourceIndex)).length
})

const activeDiff = computed(() => {
  const item = active.value
  return item ? diffStates.value.get(item.sourceIndex) ?? null : null
})
const diffLoading = computed(() => activeDiff.value?.loading ?? false)
const diffOps = computed(() => activeDiff.value?.data?.ops ?? null)
const diffError = computed(
  () => activeDiff.value?.error || activeDiff.value?.data?.diffError || ''
)
const activeDiffRows = computed(() =>
  diffOps.value
    ? buildDiffRows(diffOps.value, { context: 1, expanded: expandedDiff.value })
    : []
)
const showDetailExcerpt = computed(() => {
  if (diffLoading.value) return false
  return !(diffOps.value && diffOps.value.length > 0)
})

watch([() => props.bookId, () => props.chapterUrl], () => {
  pastedText.value = ''
  clearFile()
})

watch(filter, () => {
  displayLimit.value = 500
})

// 预览结果绑定的是当时那份文本；改了输入就作废，避免拿旧选择导入新内容。
watch([pastedText, file], () => {
  if (preview.value) clearPreview()
})

function countChars(text: string) {
  // 按 Unicode 码点计数，和后端 chars().count() 同口径；不额外分配数组。
  let count = 0
  for (let i = 0; i < text.length; i += 1) {
    const code = text.charCodeAt(i)
    if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length) {
      const next = text.charCodeAt(i + 1)
      if (next >= 0xdc00 && next <= 0xdfff) i += 1
    }
    count += 1
  }
  return count
}

function formatCount(value: number | null) {
  return value == null ? '统计字数…' : `${value.toLocaleString('zh-CN')} 字`
}

async function countFileChars(target: File): Promise<number | null> {
  try {
    const bytes = new Uint8Array(await target.arrayBuffer())
    let text: string
    if (bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xfe) {
      text = new TextDecoder('utf-16le').decode(bytes.subarray(2))
    } else if (bytes.length >= 2 && bytes[0] === 0xfe && bytes[1] === 0xff) {
      text = new TextDecoder('utf-16be').decode(bytes.subarray(2))
    } else {
      try {
        text = new TextDecoder('utf-8', { fatal: true }).decode(bytes)
      } catch {
        text = new TextDecoder('gb18030').decode(bytes)
      }
    }
    return countChars(text)
  } catch {
    return null
  }
}

function compareLabel(item: ChapterImportPreviewChapter) {
  if (item.status === 'empty') return '空章节'
  if (item.status !== 'matched') return '未匹配'
  if (!item.cached) return '未缓存'
  return item.changed ? '有差异' : '一致'
}

function compareClass(item: ChapterImportPreviewChapter) {
  if (item.status === 'empty') return 'empty'
  if (item.status !== 'matched') return 'unmatched'
  if (!item.cached) return 'uncached'
  return item.changed ? 'changed' : 'same'
}

function matchLabel(item: ChapterImportPreviewChapter) {
  if (item.matchKind === 'title') return '标题一致'
  if (item.matchKind === 'number') return '序号一致'
  if (item.matchKind === 'order') return '按顺序'
  if (item.matchKind === 'single') return '当前章节'
  return '未命中'
}

function clearPreview() {
  preview.value = null
  selected.value = new Set()
  active.value = null
  error.value = ''
  filter.value = 'all'
  displayLimit.value = 500
  diffStates.value = new Map()
  expandedDiff.value = new Map()
}

function switchMode(next: 'book' | 'single') {
  if (mode.value === next) return
  mode.value = next
  clearPreview()
}

function pickFile() {
  fileInput.value?.click()
}

function onFileChange(event: Event) {
  const input = event.target as HTMLInputElement
  const next = input.files?.[0] ?? null
  file.value = next
  fileChars.value = null
  if (next) {
    pastedText.value = ''
    void countFileChars(next).then((chars) => {
      if (file.value === next) fileChars.value = chars
    })
  }
  clearPreview()
}

function clearFile() {
  file.value = null
  fileChars.value = null
  if (fileInput.value) fileInput.value.value = ''
  clearPreview()
}

function setDiffState(sourceIndex: number, state: DiffState) {
  const next = new Map(diffStates.value)
  next.set(sourceIndex, state)
  diffStates.value = next
}

async function ensureDiff(item: ChapterImportPreviewChapter) {
  if (item.status !== 'matched' || !item.cached || !item.changed) return
  const existing = diffStates.value.get(item.sourceIndex)
  if (existing && (existing.loading || existing.data)) return
  const previewId = preview.value?.previewId
  if (!previewId) {
    setDiffState(item.sourceIndex, {
      loading: false,
      data: null,
      error: '正文内容较大，未生成逐章差异；可以缩小导入范围后重新预览。',
    })
    return
  }
  setDiffState(item.sourceIndex, { loading: true, data: null, error: '' })
  try {
    const data = await getChapterImportDiff(previewId, item.sourceIndex)
    setDiffState(item.sourceIndex, { loading: false, data, error: '' })
  } catch (e) {
    setDiffState(item.sourceIndex, {
      loading: false,
      data: null,
      error: (e as Error).message || '对比正文失败',
    })
  }
}

function setActive(item: ChapterImportPreviewChapter) {
  active.value = item
  expandedDiff.value = new Map()
  void ensureDiff(item)
}

function expandDiff(opsStart: number) {
  const next = new Map(expandedDiff.value)
  next.set(opsStart, (next.get(opsStart) ?? 0) + 10)
  expandedDiff.value = next
}

function applySelection(mutator: (set: Set<number>) => void) {
  const next = new Set(selected.value)
  mutator(next)
  selected.value = next
}

function toggle(item: ChapterImportPreviewChapter) {
  if (item.status !== 'matched') return
  applySelection((set) => {
    if (set.has(item.sourceIndex)) set.delete(item.sourceIndex)
    else set.add(item.sourceIndex)
  })
}

function selectChanged() {
  selected.value = new Set(
    (preview.value?.chapters ?? [])
      .filter((item) => item.status === 'matched' && item.cached && item.changed)
      .map((item) => item.sourceIndex)
  )
}

function selectAllMatched() {
  selected.value = new Set(
    (preview.value?.chapters ?? [])
      .filter((item) => item.status === 'matched')
      .map((item) => item.sourceIndex)
  )
}

function clearSelection() {
  selected.value = new Set()
}

function paramsFor(nextMode: ChapterImportMode): ChapterImportParams {
  const createdAt =
    kind.value === 'imported' && importAt.value
      ? new Date(importAt.value).getTime()
      : undefined
  return {
    bookId: props.bookId ?? '',
    mode: nextMode,
    chapterUrl: props.chapterUrl,
    chapterIndex: props.chapterIndex,
    chapterTitle: props.chapterTitle,
    kind: kind.value,
    note: note.value,
    makeCurrent: makeCurrent.value,
    createdAt: Number.isFinite(createdAt) ? createdAt : undefined,
    file: file.value,
    text: file.value ? undefined : pastedText.value,
  }
}

async function runPreview() {
  if (!canPreview.value) {
    error.value =
      mode.value === 'single' ? '请先选择 TXT 文件或粘贴正文' : '请先选择 TXT 文件或粘贴文本'
    return
  }
  previewing.value = true
  error.value = ''
  try {
    const result = await previewChapterImport(paramsFor(mode.value))
    preview.value = result
    selected.value = new Set(
      result.chapters
        .filter((item) => item.status === 'matched' && item.cached && item.changed)
        .map((item) => item.sourceIndex)
    )
    diffStates.value = new Map()
    expandedDiff.value = new Map()
    active.value =
      result.chapters.find((item) => item.status === 'matched' && item.cached && item.changed) ??
      result.chapters.find((item) => item.status === 'matched') ??
      result.chapters[0] ??
      null
    if (active.value) void ensureDiff(active.value)
    filter.value = 'all'
    displayLimit.value = 500
  } catch (e) {
    preview.value = null
    error.value = (e as Error).message || '解析 TXT 失败'
  } finally {
    previewing.value = false
  }
}

async function doImport() {
  const current = preview.value
  if (!current || selectedCount.value === 0 || importing.value) return
  importing.value = true
  error.value = ''
  try {
    const params = paramsFor(current.mode === 'auto' ? mode.value : current.mode)
    if (current.previewId) {
      // 复用预览暂存：导入只带 previewId，整本文本不再二次上传
      params.previewId = current.previewId
      params.file = null
      params.text = undefined
    }
    const result = await importChapterText(params, [...selected.value])
    const includesCurrent = Boolean(
      props.chapterUrl &&
        current.chapters.some(
          (item) =>
            selected.value.has(item.sourceIndex) && item.chapterUrl === props.chapterUrl
        )
    )
    emit('imported', {
      result,
      makeCurrent: makeCurrent.value,
      includesCurrent,
    })
  } catch (e) {
    const message = (e as Error).message || '导入失败'
    error.value = message
    if (current.previewId && message.includes('请重新解析')) {
      // 预览已过期或与当前书籍不一致：作废 previewId，勾选保留，
      // 重试时回退到重新上传当前文件/文本（用户也可重新预览）。
      preview.value = { ...current, previewId: null }
    }
  } finally {
    importing.value = false
  }
}
</script>

<style scoped>
.import-panel {
  height: 100%;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.import-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  flex-shrink: 0;
}

.head-text h3 {
  margin: 0;
  font-size: var(--text-lg);
}

.head-text p {
  margin: 4px 0 0;
  font-size: var(--text-xs);
  opacity: 0.65;
  line-height: 1.6;
}

.ghost-btn {
  padding: 6px 12px;
  border-radius: 8px;
  font-size: var(--text-sm);
  cursor: pointer;
  border: 1px solid var(--color-border, rgba(128, 128, 128, 0.25));
  background: transparent;
  color: inherit;
  white-space: nowrap;
}

.ghost-btn:disabled {
  opacity: 0.45;
  cursor: default;
}

.primary-btn {
  padding: 7px 16px;
  border-radius: 8px;
  border: none;
  background: #2563eb;
  color: var(--color-text-inverse);
  font-size: var(--text-sm);
  cursor: pointer;
  white-space: nowrap;
}

.primary-btn:disabled {
  opacity: 0.45;
  cursor: default;
}

.text-btn {
  border: none;
  background: transparent;
  color: inherit;
  padding: 0;
  font-size: var(--text-xs);
  opacity: 0.65;
  cursor: pointer;
  text-decoration: underline;
  text-underline-offset: 2px;
}

.text-btn:hover {
  opacity: 1;
}

.mode-tabs {
  display: inline-flex;
  gap: 4px;
  padding: 3px;
  border-radius: 10px;
  background: var(--color-bg-hover);
  align-self: flex-start;
  flex-shrink: 0;
}

.mode-tabs button {
  padding: 5px 14px;
  border: none;
  border-radius: 8px;
  background: transparent;
  color: inherit;
  font-size: var(--text-sm);
  cursor: pointer;
  opacity: 0.7;
}

.mode-tabs button.active {
  background: var(--color-bg-elevated);
  opacity: 1;
  box-shadow: var(--shadow-sm, 0 1px 2px rgba(0, 0, 0, 0.12));
}

.file-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  flex-shrink: 0;
}

.file-input {
  display: none;
}

.file-name {
  min-width: 0;
  max-width: 260px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
}

.file-size,
.hint {
  font-size: var(--text-xs);
  opacity: 0.6;
}

.text-input {
  width: 100%;
  min-height: 84px;
  max-height: 160px;
  resize: vertical;
  border-radius: 8px;
  border: 1px solid rgba(128, 128, 128, 0.25);
  background: transparent;
  color: inherit;
  padding: 8px 10px;
  font: inherit;
  font-size: var(--text-sm);
  flex-shrink: 0;
}

.option-row {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  align-items: center;
  flex-shrink: 0;
  font-size: var(--text-sm);
}

.option {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.option select,
.option input {
  border-radius: 8px;
  border: 1px solid rgba(128, 128, 128, 0.25);
  background: transparent;
  color: inherit;
  padding: 5px 8px;
  font: inherit;
  font-size: var(--text-sm);
}

.check {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: var(--text-sm);
  opacity: 0.8;
}

.note-input {
  flex: 1;
  min-width: 180px;
  border-radius: 8px;
  border: 1px solid rgba(128, 128, 128, 0.25);
  background: transparent;
  color: inherit;
  padding: 5px 8px;
  font: inherit;
  font-size: var(--text-sm);
}

.action-row,
.import-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-shrink: 0;
}

.error-text {
  color: var(--color-danger);
  font-size: var(--text-sm);
  flex-shrink: 0;
}

.preview-area {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
  border-top: 1px solid rgba(128, 128, 128, 0.12);
  padding-top: 8px;
}

.preview-summary {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  font-size: var(--text-xs);
  opacity: 0.72;
  flex-shrink: 0;
}

.summary-selected {
  color: #2563eb;
  opacity: 1;
}

.summary-changed {
  color: #2563eb;
  opacity: 1;
}

.summary-uncached {
  color: #b45309;
  opacity: 1;
}

.cache-hint {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  padding: 6px 10px;
  border-radius: 8px;
  background: rgba(245, 158, 11, 0.12);
  color: #b45309;
  font-size: var(--text-xs);
  line-height: 1.6;
  flex-shrink: 0;
}

.same-hint {
  font-size: var(--text-xs);
  opacity: 0.6;
  flex-shrink: 0;
}

.preview-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  flex-wrap: wrap;
  flex-shrink: 0;
}

.filter-tabs {
  display: flex;
  gap: 6px;
}

.filter-tabs button {
  border: none;
  background: transparent;
  color: inherit;
  font-size: var(--text-xs);
  padding: 3px 8px;
  border-radius: 999px;
  cursor: pointer;
  opacity: 0.6;
}

.filter-tabs button.active {
  background: var(--color-bg-hover);
  opacity: 1;
}

.selection-actions {
  display: flex;
  gap: 12px;
}

.preview-body {
  flex: 1;
  min-height: 0;
  display: grid;
  grid-template-columns: minmax(0, 1.05fr) minmax(0, 1fr);
  gap: 10px;
}

.chapter-list,
.chapter-detail {
  min-height: 0;
  overflow-y: auto;
  border: 1px solid rgba(128, 128, 128, 0.15);
  border-radius: 10px;
  padding: 6px;
}

.chapter-row {
  display: grid;
  grid-template-columns: 18px minmax(0, 1fr) auto;
  gap: 8px;
  align-items: center;
  padding: 6px 8px;
  border-radius: 8px;
  cursor: pointer;
  outline: none;
}

.chapter-row:hover,
.chapter-row.active {
  background: var(--color-bg-hover);
}

.chapter-row.disabled {
  cursor: default;
  opacity: 0.55;
}

.chapter-row input {
  width: 15px;
  height: 15px;
}

.chapter-main {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.chapter-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
}

.chapter-sub {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-xs);
  opacity: 0.62;
}

.match-badge {
  flex-shrink: 0;
  padding: 0 5px;
  border-radius: 999px;
  background: rgba(128, 128, 128, 0.15);
}

.match-badge.title,
.match-badge.number {
  color: #16a34a;
  background: rgba(34, 197, 94, 0.14);
}

.match-badge.order {
  color: #b45309;
  background: rgba(245, 158, 11, 0.16);
}

.match-badge.none {
  color: #dc2626;
  background: rgba(239, 68, 68, 0.14);
}

.chapter-side {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 2px;
  font-size: var(--text-xs);
}

.status {
  padding: 0 6px;
  border-radius: 999px;
  background: rgba(128, 128, 128, 0.15);
}

.status.matched {
  color: #16a34a;
  background: rgba(34, 197, 94, 0.14);
}

.status.unmatched {
  color: #dc2626;
  background: rgba(239, 68, 68, 0.14);
}

.status.empty {
  color: #b45309;
  background: rgba(245, 158, 11, 0.16);
}

.status.changed {
  color: #2563eb;
  background: rgba(37, 99, 235, 0.12);
}

.status.same {
  color: #16a34a;
  background: rgba(34, 197, 94, 0.12);
}

.status.uncached {
  color: #b45309;
  background: rgba(245, 158, 11, 0.16);
}

.chars {
  opacity: 0.55;
}

.show-more {
  width: 100%;
  margin-top: 4px;
  padding: 6px 0;
  border: 1px dashed rgba(128, 128, 128, 0.35);
  border-radius: 8px;
  background: transparent;
  color: inherit;
  font-size: var(--text-xs);
  opacity: 0.7;
  cursor: pointer;
}

.detail-title {
  font-size: var(--text-sm);
  font-weight: 600;
  padding: 4px 6px 0;
}

.detail-meta {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
  align-items: center;
  padding: 4px 6px 8px;
  font-size: var(--text-xs);
  opacity: 0.65;
}

.detail-excerpt {
  margin: 0;
  padding: 4px 6px 8px;
  font: inherit;
  font-size: var(--text-sm);
  line-height: 1.8;
  white-space: pre-wrap;
  word-break: break-word;
  opacity: 0.85;
}

.detail-note {
  padding: 2px 6px 8px;
  font-size: var(--text-sm);
  line-height: 1.6;
  opacity: 0.65;
}

.detail-note.error {
  color: var(--color-danger);
  opacity: 0.9;
}

.detail-block {
  padding: 0 6px 6px;
}

.detail-label {
  margin-bottom: 4px;
  font-size: var(--text-xs);
  opacity: 0.55;
}

.detail-diff {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 2px 6px 8px;
}

.diff-line {
  display: flex;
  gap: 6px;
  padding: 1px 0;
  font-size: var(--text-sm);
  line-height: 1.7;
  white-space: pre-wrap;
  word-break: break-word;
}

.diff-line.equal {
  opacity: 0.55;
}

.diff-line.del {
  background: rgba(239, 68, 68, 0.1);
}

.diff-line.add {
  background: rgba(34, 197, 94, 0.1);
}

.diff-sign {
  width: 12px;
  flex-shrink: 0;
  text-align: center;
  opacity: 0.7;
  user-select: none;
}

.diff-line.del .diff-sign {
  color: var(--color-danger);
}

.diff-line.add .diff-sign {
  color: #22c55e;
}

.diff-collapse {
  width: 100%;
  margin: 2px 0;
  padding: 4px 0;
  border: 1px dashed rgba(128, 128, 128, 0.35);
  border-radius: 8px;
  background: transparent;
  color: inherit;
  opacity: 0.6;
  font-size: var(--text-xs);
  cursor: pointer;
  text-align: center;
}

.empty {
  display: flex;
  align-items: center;
  justify-content: center;
  opacity: 0.55;
  font-size: var(--text-sm);
}

.empty.small {
  padding: 20px;
}

@media (max-width: 767px) {
  .import-panel {
    overflow-y: auto;
  }
  .preview-body {
    grid-template-columns: 1fr;
    grid-template-rows: minmax(160px, 40vh) auto;
  }
  .chapter-list {
    max-height: 40vh;
  }
  .chapter-detail {
    max-height: 220px;
  }
  .action-row,
  .import-footer {
    align-items: flex-start;
    flex-direction: column;
  }
}
</style>
