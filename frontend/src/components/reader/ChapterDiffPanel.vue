<template>
  <Teleport to="body">
    <Transition name="fade">
      <div v-if="modelValue" class="modal-overlay" @click="close"></div>
    </Transition>
    <Transition :name="isMobileLayout ? 'slide-right' : 'scale'">
      <div v-if="modelValue" class="modal-container" @click.self="close">
        <div class="diff-modal">
          <div class="modal-head">
            <div class="head-main">
              <h2>本章改动</h2>
              <p class="head-sub">{{ chapterTitle }}</p>
            </div>
            <div class="head-actions">
              <span v-if="stats" class="diff-stats">
                <span class="stat-add">+{{ stats.added }} 段</span>
                <span class="stat-del">-{{ stats.removed }} 段</span>
                <span class="stat-mod">~{{ stats.modified }} 段</span>
              </span>
              <button class="ghost-btn" @click="emit('edit')">编辑本章</button>
              <button class="danger-btn" @click="discardEdit">放弃本章修改</button>
              <button class="close-btn" @click="close">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                  <path d="M18 6 6 18M6 6l12 12" />
                </svg>
              </button>
            </div>
          </div>

          <div v-if="loading" class="diff-loading">正在计算差异...</div>
          <div v-else-if="!ops?.length" class="diff-loading">本章没有差异</div>
          <div v-else class="diff-body">
            <template v-for="(row, rowIndex) in rows" :key="rowIndex">
              <!-- 折叠的未变更区域：点击展开 15 段 -->
              <button
                v-if="row.kind === 'collapse'"
                class="diff-collapse"
                @click="expandCollapse(row.opsStart)"
              >
                ⋯ 展开 15 段（还有 {{ row.opsCount }} 段未显示）
              </button>

                <!-- 未变段 -->
              <div v-else-if="row.op.type === 'equal'" class="diff-row equal">
                <span class="diff-gutter"></span>
                <span class="diff-text">{{ row.op.editedText }}</span>
              </div>

              <!-- 删除段（基准里有、编辑版没有） -->
              <div v-else-if="row.op.type === 'del'" class="diff-row del">
                <span class="diff-gutter">-</span>
                <span class="diff-text">{{ row.op.baseText }}</span>
              </div>

              <!-- 新增段 -->
              <div v-else-if="row.op.type === 'add'" class="diff-row add">
                <span class="diff-gutter">+</span>
                <span class="diff-text">{{ row.op.editedText }}</span>
              </div>

              <!-- 段内改动：上行基准（红），下行编辑版（绿，新增部分加粗底色） -->
              <template v-else>
                <div class="diff-row del">
                  <span class="diff-gutter">-</span>
                  <span class="diff-text">{{ row.op.baseText }}</span>
                </div>
                <div class="diff-row add">
                  <span class="diff-gutter">+</span>
                  <span class="diff-text">
                    <template v-for="(part, partIndex) in inlineParts(row.op)" :key="partIndex">
                      <span v-if="part.added" class="diff-ins-strong">{{ part.text }}</span>
                      <span v-else>{{ part.text }}</span>
                    </template>
                  </span>
                </div>
              </template>
            </template>
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
import { deleteChapterEdit } from '../../api/chapterEdit'
import { useEscClose } from '../../composables/useEscClose'
import { useMobileLayout } from '../../composables/useMobileLayout'
import {
  buildDiffRows,
  diffStats,
  inlineDiff,
  type ParagraphDiffOp,
} from '../../utils/chapterDiff'

const props = defineProps<{
  modelValue: boolean
  /** 当前章的 diff 操作序列（父组件的 chapterDiffCache 供给） */
  ops: ParagraphDiffOp[] | null
  loading: boolean
}>()
const emit = defineEmits<{
  'update:modelValue': [value: boolean]
  /** 「编辑本章」：关掉面板开编辑器 */
  edit: []
  /** 放弃修改后（父组件重载本章与 diff） */
  changed: []
}>()

const store = useReaderStore()
const appStore = useAppStore()
const { isMobileLayout } = useMobileLayout()
useEscClose(toRef(props, 'modelValue'), () => emit('update:modelValue', false))

const chapterTitle = computed(() => store.currentChapter?.title || '')
const stats = computed(() => (props.ops?.length ? diffStats(props.ops) : null))

// 折叠块的展开进度：key = 折叠块起始 op 下标，value = 已展开段数
const expanded = ref(new Map<number, number>())

watch(
  () => props.modelValue,
  (open) => {
    if (open) expanded.value = new Map()
  }
)

const rows = computed(() => buildDiffRows(props.ops ?? [], { expanded: expanded.value }))

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

function close() {
  emit('update:modelValue', false)
}

async function discardEdit() {
  const chapter = store.currentChapter
  const bookId = store.book?.bookId
  if (!chapter || !bookId) return
  if (!confirm('确定放弃本章的修改，恢复上游版本吗？')) return
  try {
    await deleteChapterEdit(bookId, chapter.url)
    emit('update:modelValue', false)
    emit('changed')
    appStore.showToast('已恢复上游版本', 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || '操作失败', 'error')
  }
}
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

.diff-modal {
  width: min(960px, 100%);
  height: 82vh;
  display: flex;
  flex-direction: column;
  background: var(--color-bg-elevated);
  border-radius: 24px;
  padding: 24px;
  box-shadow: var(--shadow-xl);
}

.modal-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
  margin-bottom: 16px;
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
  gap: 10px;
  flex-wrap: wrap;
  justify-content: flex-end;
}

.diff-stats {
  display: inline-flex;
  gap: 8px;
  font-size: var(--text-xs);
  margin-right: 4px;
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

.ghost-btn,
.danger-btn {
  padding: 6px 14px;
  border-radius: 8px;
  font-size: var(--text-sm);
  cursor: pointer;
  border: 1px solid var(--color-border, rgba(128, 128, 128, 0.25));
  background: transparent;
  color: inherit;
}

.danger-btn {
  border-color: rgba(239, 68, 68, 0.4);
  color: #ef4444;
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

.diff-loading {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  opacity: 0.55;
}

.diff-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
  border: 1px solid rgba(128, 128, 128, 0.15);
  border-radius: 12px;
  padding: 8px 0;
  font-size: var(--text-sm);
  line-height: 1.8;
}

.diff-row {
  display: flex;
  gap: 10px;
  padding: 2px 14px 2px 8px;
  white-space: pre-wrap;
  word-break: break-word;
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
  width: calc(100% - 16px);
  margin: 4px 8px;
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

.diff-collapse:hover {
  opacity: 1;
  background: var(--color-bg-hover);
}

@media (max-width: 767px) {
  .modal-overlay {
    display: none;
  }
  .modal-container {
    padding: 0;
  }
  .diff-modal {
    width: 100%;
    height: 100%;
    border-radius: 0;
    padding: 16px;
    padding-top: calc(16px + var(--safe-area-top));
    padding-bottom: calc(16px + var(--safe-area-bottom));
  }
  .head-actions {
    gap: 6px;
  }
}
</style>
