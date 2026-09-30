<template>
  <div class="code-editor">
    <div class="gutter" aria-hidden="true">
      <div ref="gutterInnerRef" class="gutter-inner">
        <div v-for="n in lineCount" :key="n" class="gutter-line">{{ n }}</div>
      </div>
    </div>
    <div class="code-scroll">
      <pre class="highlight-layer" aria-hidden="true"><code ref="highlightRef" v-html="highlightedHtml"></code></pre>
      <textarea
        ref="textareaRef"
        class="input-layer"
        :value="modelValue"
        spellcheck="false"
        @input="$emit('update:modelValue', ($event.target as HTMLTextAreaElement).value)"
        @scroll="syncScroll"
        @keydown="onKeydown"
      ></textarea>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { highlightJsonToHtml } from '../../utils/jsonHighlight'

const props = defineProps<{
  modelValue: string
}>()

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

const textareaRef = ref<HTMLTextAreaElement | null>(null)
const highlightRef = ref<HTMLElement | null>(null)
const gutterInnerRef = ref<HTMLElement | null>(null)

const highlightedHtml = computed(() => highlightJsonToHtml(props.modelValue))
const lineCount = computed(() => props.modelValue.split('\n').length)

// 滚动以 textarea 为准：高亮层与行号列跟随
function syncScroll() {
  const ta = textareaRef.value
  if (!ta) return
  if (highlightRef.value) {
    highlightRef.value.style.transform = `translate(${-ta.scrollLeft}px, ${-ta.scrollTop}px)`
  }
  if (gutterInnerRef.value) {
    gutterInnerRef.value.style.transform = `translateY(${-ta.scrollTop}px)`
  }
}

// Tab 缩进两个空格（默认行为是移焦点，编辑 JSON 时反直觉）
function onKeydown(e: KeyboardEvent) {
  if (e.key !== 'Tab') return
  const ta = textareaRef.value
  if (!ta) return
  e.preventDefault()
  const { selectionStart: start, selectionEnd: end, value } = ta
  const next = `${value.slice(0, start)}  ${value.slice(end)}`
  emit('update:modelValue', next)
  requestAnimationFrame(() => {
    ta.selectionStart = ta.selectionEnd = start + 2
  })
}
</script>

<style scoped>
.code-editor {
  display: flex;
  height: 100%;
  min-height: 0;
  background: #181614;
}

.gutter {
  flex-shrink: 0;
  width: 46px;
  overflow: hidden;
  padding: 16px 0;
  border-right: 1px solid rgba(255, 255, 255, 0.07);
  background: #1d1b19;
  user-select: none;
}

.gutter-line {
  padding-right: 10px;
  text-align: right;
  color: #6e655c;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: var(--text-xs);
  line-height: 1.55;
}

.code-scroll {
  position: relative;
  flex: 1;
  min-width: 0;
}

/* 高亮层与输入层必须逐字重叠：字体/字号/行高/内边距完全一致 */
.highlight-layer,
.input-layer {
  position: absolute;
  inset: 0;
  margin: 0;
  padding: 16px;
  border: none;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: var(--text-xs);
  line-height: 1.55;
  white-space: pre;
  tab-size: 2;
  overflow-wrap: normal;
}

.highlight-layer {
  overflow: hidden;
  pointer-events: none;
  color: #d4cfc9;
}

/* transform 只挪 code 内容，pre 外框固定（否则滚动区域会跟着跑） */
.highlight-layer code {
  display: block;
  font: inherit;
  will-change: transform;
}

.input-layer {
  resize: none;
  outline: none;
  background: transparent;
  color: transparent;
  caret-color: #f4ede4;
  overflow: auto;
}

.input-layer::selection {
  background: rgba(201, 127, 58, 0.35);
  color: transparent;
}

/* 调色参考 VSCode Dark+：键名浅蓝、字符串橙、数字浅绿、关键字蓝 */
.highlight-layer :deep(.tok-key) {
  color: #9cdcfe;
}

.highlight-layer :deep(.tok-str) {
  color: #ce9178;
}

.highlight-layer :deep(.tok-num) {
  color: #b5cea8;
}

.highlight-layer :deep(.tok-kw) {
  color: #569cd6;
}

/* 书源模板占位符：{{js}} 亮黄、{key}/{page} 青绿 */
.highlight-layer :deep(.tok-js) {
  color: #dcdcaa;
  background: rgba(220, 220, 170, 0.08);
  border-radius: 3px;
}

.highlight-layer :deep(.tok-var) {
  color: #4ec9b0;
  background: rgba(78, 201, 176, 0.1);
  border-radius: 3px;
}
</style>
