<template>
  <Teleport to="body">
    <Transition name="fade">
      <div v-if="modelValue" class="modal-overlay" @click="close"></div>
    </Transition>
    <Transition :name="isMobileLayout ? 'slide-right' : 'scale'">
      <div v-if="modelValue" class="modal-container" @click.self="close">
        <div class="editor-modal">
          <div class="modal-head">
            <div>
              <h2>编辑本章正文</h2>
              <p>{{ chapterTitle }}</p>
            </div>
            <button class="close-btn" @click="close">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
            </button>
          </div>

          <textarea
            ref="editorRef"
            v-model="draft"
            class="editor-textarea"
            placeholder="正文内容，按换行分段"
            spellcheck="false"
          ></textarea>

          <div class="modal-foot">
            <div class="foot-left">
              <button v-if="hasEdit" class="danger-btn" :disabled="saving" @click="restoreUpstream">
                恢复上游版本
              </button>
              <span v-if="hasEdit" class="edited-hint">
                本章已编辑，{{ isUpstreamView ? '这里仍显示你的修改版' : '正在显示你的修改版' }}
              </span>
            </div>
            <div class="foot-right">
              <button class="ghost-btn" :disabled="saving" @click="close">取消</button>
              <button class="primary-btn" :disabled="saving" @click="save">
                {{ saving ? '保存中...' : '保存修改' }}
              </button>
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, toRef, watch } from 'vue'
import { useReaderStore } from '../../stores/reader'
import { useAppStore } from '../../stores/app'
import { deleteChapterEdit, getChapterEdit, saveChapterEdit } from '../../api/chapterEdit'
import { useEscClose } from '../../composables/useEscClose'
import { useMobileLayout } from '../../composables/useMobileLayout'

const props = defineProps<{ modelValue: boolean }>()
const emit = defineEmits<{
  'update:modelValue': [value: boolean]
  /** 保存或恢复上游版本后触发（父组件负责重载本章与 diff） */
  changed: []
}>()

const store = useReaderStore()
const appStore = useAppStore()
const { isMobileLayout } = useMobileLayout()
useEscClose(toRef(props, 'modelValue'), () => emit('update:modelValue', false))

const draft = ref('')
const saving = ref(false)
const editorRef = ref<HTMLTextAreaElement>()
/** 本次编辑基于哪个版本；保存时作为 diff 基准 */
const baseVersionId = ref<number | undefined>()

const chapterTitle = computed(() => store.currentChapter?.title || '')
const isUpstreamView = computed(() => store.config.contentVersion === 'upstream')
const hasEdit = computed(() => {
  const chapter = store.currentChapter
  return !!chapter && store.editedChapterKeys.has(chapter.url)
})

watch(
  () => props.modelValue,
  async (open) => {
    if (!open) return
    // 默认预填当前显示内容；正在读上游原版时要取编辑版，否则一保存就把
    // 上游正文盖成编辑版，用户的修改就丢了
    const displayed = store.content || ''
    draft.value = displayed
    const chapter = store.currentChapter
    const bookId = store.book?.bookId
    baseVersionId.value = store.chapterVersions.get(store.currentIndex)?.versionId
    if (isUpstreamView.value && chapter && bookId) {
      const edit = await getChapterEdit(bookId, chapter.url).catch(() => null)
      // 等待期间用户可能已经动手改字，别覆盖
      if (props.modelValue && draft.value === displayed && edit?.content != null) {
        draft.value = edit.content
        baseVersionId.value = edit.versionId
      }
    }
    await nextTick()
    editorRef.value?.focus()
  }
)

function close() {
  if (saving.value) return
  emit('update:modelValue', false)
}

async function save() {
  const chapter = store.currentChapter
  const bookId = store.book?.bookId
  if (!chapter || !bookId) return
  saving.value = true
  try {
    await saveChapterEdit({
      bookId,
      chapterUrl: chapter.url,
      chapterIndex: store.currentIndex,
      chapterTitle: chapter.title,
      content: draft.value,
      baseVersionId: baseVersionId.value,
    })
    // 在上游原版界面保存后切回编辑版，让用户立刻看到保存结果；
    // 版本切换的 watcher 会负责重载本章
    const switchedToEdited = isUpstreamView.value
    if (switchedToEdited) {
      store.updateConfig('contentVersion', 'edited')
    }
    emit('update:modelValue', false)
    if (!switchedToEdited) {
      emit('changed')
    }
    appStore.showToast('已保存本章修改', 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || '保存失败', 'error')
  } finally {
    saving.value = false
  }
}

async function restoreUpstream() {
  const chapter = store.currentChapter
  const bookId = store.book?.bookId
  if (!chapter || !bookId) return
  if (!confirm('确定放弃本章的修改，恢复上游版本吗？')) return
  saving.value = true
  try {
    await deleteChapterEdit(bookId, chapter.url)
    emit('update:modelValue', false)
    emit('changed')
    appStore.showToast('已恢复上游版本', 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || '恢复失败', 'error')
  } finally {
    saving.value = false
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

.editor-modal {
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

.modal-head h2 {
  margin: 0;
  font-size: var(--text-lg);
}

.modal-head p {
  margin: 4px 0 0;
  font-size: var(--text-sm);
  opacity: 0.6;
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

.editor-textarea {
  flex: 1;
  min-height: 0;
  width: 100%;
  box-sizing: border-box;
  resize: none;
  background: var(--color-bg);
  color: inherit;
  border: 1px solid var(--color-border, rgba(128, 128, 128, 0.2));
  border-radius: 12px;
  padding: 16px;
  font-size: var(--text-base);
  line-height: 1.8;
  font-family: inherit;
  outline: none;
}

.editor-textarea:focus {
  border-color: var(--color-primary);
}

.modal-foot {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  margin-top: 16px;
  flex-shrink: 0;
}

.foot-left {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.edited-hint {
  font-size: var(--text-xs);
  opacity: 0.6;
}

.foot-right {
  display: flex;
  gap: 10px;
}

.ghost-btn,
.primary-btn,
.danger-btn {
  padding: 8px 18px;
  border-radius: 8px;
  font-size: var(--text-sm);
  cursor: pointer;
  border: 1px solid var(--color-border, rgba(128, 128, 128, 0.25));
  background: transparent;
  color: inherit;
}

.primary-btn {
  background: var(--color-primary);
  border-color: var(--color-primary);
  color: #fff;
}

.danger-btn {
  border-color: rgba(239, 68, 68, 0.4);
  color: #ef4444;
}

.ghost-btn:disabled,
.primary-btn:disabled,
.danger-btn:disabled {
  opacity: 0.5;
  cursor: default;
}

@media (max-width: 767px) {
  .modal-overlay {
    display: none;
  }
  .modal-container {
    padding: 0;
  }
  .editor-modal {
    width: 100%;
    height: 100%;
    border-radius: 0;
    padding: 16px;
    padding-top: calc(16px + var(--safe-area-top));
    padding-bottom: calc(16px + var(--safe-area-bottom));
  }
}
</style>
