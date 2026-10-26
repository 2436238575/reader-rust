<template>
  <div class="rule-manager" :style="{ background: theme.popup, color: theme.fontColor }">
    <div class="rule-header">
      <h3>关键词高亮</h3>
      <div class="header-actions">
        <button class="add-btn" @click="openEdit()">+ 新增关键词</button>
        <button class="close-btn" @click="store.backPanel()">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="m15 18-6-6 6-6" />
          </svg>
        </button>
      </div>
    </div>

    <div class="rule-list">
      <div v-if="!marksStore.keywordRules.length" class="empty">
        暂无关键词
        <p>命中正文的词会以所选底色常驻标出</p>
      </div>
      <div
        v-else
        v-for="rule in marksStore.keywordRules"
        :key="rule.id"
        class="rule-item"
        :class="{ disabled: !rule.enabled }"
      >
        <div class="rule-info">
          <div class="rule-name-row">
            <span class="kw-swatch" :style="{ background: rule.color }"></span>
            <span class="rule-name">{{ rule.keyword }}</span>
          </div>
          <div class="rule-scope">{{ rule.scope === 'book' ? '范围: 当前书籍' : '范围: 全部书籍' }}</div>
        </div>
        <div class="rule-ops">
          <label class="switch">
            <input type="checkbox" :checked="rule.enabled" @change="toggleRule(rule)" />
            <span class="slider"></span>
          </label>
          <button class="op-btn" @click="openEdit(rule)">编辑</button>
          <button class="op-btn delete" @click="handleDelete(rule)">删除</button>
        </div>
      </div>
    </div>

    <Transition name="fade">
      <div v-if="editing" class="edit-overlay" :style="{ background: theme.popup }">
        <div class="edit-header">
          <h4>{{ editing.isNew ? '新增关键词' : '编辑关键词' }}</h4>
        </div>
        <div class="edit-body">
          <div class="form-item">
            <label>关键词</label>
            <input v-model="editing.keyword" placeholder="要标出的词，如角色名、地名" />
          </div>
          <div class="form-item">
            <label>底色</label>
            <div class="color-row">
              <button
                v-for="color in KEYWORD_COLORS"
                :key="color"
                type="button"
                class="color-swatch"
                :class="{ active: editing.color === color }"
                :style="{ background: color }"
                @click="editing && (editing.color = color)"
              ></button>
            </div>
          </div>
          <div class="form-item">
            <label>生效范围</label>
            <div class="scope-group">
              <button
                type="button"
                class="scope-btn"
                :class="{ active: editing.scope === 'global' }"
                @click="editing && (editing.scope = 'global')"
              >
                全部书籍
              </button>
              <button
                type="button"
                class="scope-btn"
                :class="{ active: editing.scope === 'book' }"
                :disabled="!store.book"
                @click="editing && (editing.scope = 'book')"
              >
                当前书籍
              </button>
            </div>
          </div>
        </div>
        <div class="edit-footer">
          <button @click="editing = null">取消</button>
          <button class="primary" @click="handleSave">保存</button>
        </div>
      </div>
    </Transition>
  </div>
</template>

<script setup lang="ts">
import { ref, computed } from 'vue'
import { useReaderStore } from '../../stores/reader'
import { useReaderMarksStore } from '../../stores/readerMarks'
import { useReaderConfigStore } from '../../stores/readerConfig'
import { useAppStore } from '../../stores/app'
import {
  DEFAULT_KEYWORD_COLOR,
  KEYWORD_COLORS,
  newKeywordRuleId,
  type KeywordHighlightRule,
} from '../../utils/keywordHighlights'

const store = useReaderStore()
const marksStore = useReaderMarksStore()
const configStore = useReaderConfigStore()
const appStore = useAppStore()
const theme = computed(() => configStore.currentTheme)

interface EditingRule {
  isNew: boolean
  id: string
  keyword: string
  color: string
  enabled: boolean
  scope: 'global' | 'book'
  bookId?: string
}

const editing = ref<EditingRule | null>(null)

function openEdit(rule?: KeywordHighlightRule) {
  editing.value = rule
    ? { isNew: false, ...rule }
    : {
        isNew: true,
        id: newKeywordRuleId(),
        keyword: '',
        color: DEFAULT_KEYWORD_COLOR,
        enabled: true,
        // 新增默认只标当前书（最常见的意图是标记本书的角色/地名）；
        // 没打开书籍时退化为全局
        scope: store.book ? 'book' : 'global',
        bookId: store.book?.bookId,
      }
}

function handleSave() {
  const current = editing.value
  if (!current) return
  if (!current.keyword.trim()) {
    appStore.showToast('请填写关键词', 'warning')
    return
  }
  const next: KeywordHighlightRule = {
    id: current.id,
    keyword: current.keyword.trim(),
    color: current.color,
    enabled: current.enabled,
    scope: current.scope,
    bookId:
      current.scope === 'book' ? current.bookId || store.book?.bookId || undefined : undefined,
    updatedAt: Date.now(),
  }
  if (next.scope === 'book' && !next.bookId) {
    appStore.showToast('当前没有打开的书籍，无法按书生效', 'warning')
    return
  }
  const rules = marksStore.keywordRules.filter((rule) => rule.id !== next.id)
  rules.push(next)
  marksStore.saveKeywordRules(rules)
  editing.value = null
  appStore.showToast('已保存', 'success')
}

function toggleRule(rule: KeywordHighlightRule) {
  const rules = marksStore.keywordRules.map((item) =>
    item.id === rule.id ? { ...item, enabled: !item.enabled, updatedAt: Date.now() } : item
  )
  marksStore.saveKeywordRules(rules)
}

function handleDelete(rule: KeywordHighlightRule) {
  if (!confirm(`确定删除关键词 "${rule.keyword}" 吗？`)) return
  marksStore.saveKeywordRules(marksStore.keywordRules.filter((item) => item.id !== rule.id))
}
</script>

<style scoped>
.rule-manager {
  display: flex;
  flex-direction: column;
  height: 100%;
  position: relative;
}

.rule-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 20px;
  border-bottom: 1px solid rgba(0, 0, 0, 0.06);
  flex-shrink: 0;
}

.rule-header h3 {
  margin: 0;
  font-size: var(--text-lg);
}

.header-actions {
  display: flex;
  gap: 12px;
  align-items: center;
}

.add-btn {
  background: var(--color-primary);
  color: white;
  border: none;
  padding: 6px 12px;
  border-radius: 6px;
  font-size: var(--text-sm);
  cursor: pointer;
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
}

.close-btn:hover {
  background: var(--color-bg-hover);
  opacity: 1;
}

.rule-list {
  flex: 1;
  overflow-y: auto;
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 12px;
  -webkit-overflow-scrolling: touch;
  overscroll-behavior: contain;
}

.empty {
  padding: 40px;
  text-align: center;
  opacity: 0.5;
  font-size: var(--text-base);
}

.rule-item {
  padding: 12px;
  border-radius: 12px;
  background: rgba(0, 0, 0, 0.03);
  border: 1px solid rgba(0, 0, 0, 0.05);
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  transition: opacity 0.2s;
}

.rule-item.disabled {
  opacity: 0.5;
}

.rule-info {
  flex: 1;
  min-width: 0;
}

.rule-name-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 4px;
}

.kw-swatch {
  width: 14px;
  height: 14px;
  border-radius: 4px;
  border: 1px solid rgba(0, 0, 0, 0.12);
  flex-shrink: 0;
}

.rule-name {
  font-weight: 600;
  font-size: var(--text-base);
  word-break: break-all;
}

.rule-scope {
  font-size: var(--text-xs);
  color: var(--color-primary);
  opacity: 0.9;
}

.rule-ops {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 8px;
}

.op-btn {
  font-size: var(--text-xs);
  background: transparent;
  border: 1px solid var(--color-border);
  padding: 2px 8px;
  border-radius: 4px;
  cursor: pointer;
  color: inherit;
}

.op-btn.delete {
  color: #ef4444;
  border-color: rgba(239, 68, 68, 0.2);
}

/* Switch style（与净化规则面板同款） */
.switch {
  position: relative;
  display: inline-block;
  width: 34px;
  height: 18px;
}
.switch input {
  opacity: 0;
  width: 0;
  height: 0;
}
.slider {
  position: absolute;
  cursor: pointer;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background-color: #ccc;
  transition: 0.4s;
  border-radius: 18px;
}
.slider:before {
  position: absolute;
  content: '';
  height: 14px;
  width: 14px;
  left: 2px;
  bottom: 2px;
  background-color: white;
  transition: 0.4s;
  border-radius: 50%;
}
input:checked + .slider {
  background-color: var(--color-primary);
}
input:checked + .slider:before {
  transform: translateX(16px);
}

.edit-overlay {
  position: absolute;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  z-index: 10;
  display: flex;
  flex-direction: column;
  padding: 20px;
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
}

.edit-header h4 {
  margin: 0 0 16px 0;
}

.edit-body {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.form-item {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.form-item label {
  font-size: var(--text-xs);
  opacity: 0.7;
}
.form-item input {
  background: rgba(0, 0, 0, 0.05);
  border: 1px solid rgba(0, 0, 0, 0.05);
  padding: 8px 12px;
  border-radius: 8px;
  color: inherit;
  outline: none;
  font-size: var(--text-base);
}

.color-row {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
}

.color-swatch {
  width: 28px;
  height: 28px;
  border-radius: 8px;
  border: 2px solid transparent;
  cursor: pointer;
  padding: 0;
}

.color-swatch.active {
  border-color: var(--color-primary);
}

.scope-group {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}

.scope-btn {
  border: 1px solid rgba(0, 0, 0, 0.1);
  background: transparent;
  color: inherit;
  padding: 8px 12px;
  border-radius: 999px;
  cursor: pointer;
  font-size: var(--text-xs);
}

.scope-btn.active {
  background: var(--color-primary);
  color: white;
  border-color: var(--color-primary);
}

.scope-btn:disabled {
  opacity: 0.4;
  cursor: default;
}

.edit-footer {
  display: flex;
  justify-content: flex-end;
  gap: 12px;
  margin-top: 20px;
}
.edit-footer button {
  padding: 8px 20px;
  border-radius: 8px;
  border: 1px solid var(--color-border);
  background: transparent;
  color: inherit;
  cursor: pointer;
}
.edit-footer button.primary {
  background: var(--color-primary);
  color: white;
  border: none;
}
</style>
