<template>
  <div class="ai-book-view">
    <div v-if="loading" class="ai-loading">
      <div class="loading-spinner"></div>
      <span>加载中...</span>
    </div>

    <div v-else-if="book && memory" class="ai-shell">
      <header class="ai-header">
        <div class="title-stack">
          <div class="title-row">
            <button class="back-btn" @click="goBack">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="m15 18-6-6 6-6" />
              </svg>
              返回
            </button>
            <h1>{{ book.name }}</h1>
          </div>
          <p>{{ book.author || '未知作者' }} · {{ progressText }}</p>
        </div>

        <div class="header-actions">
          <label class="enable-switch">
            <input type="checkbox" :checked="memory.enabled" @change="toggleEnabled" />
            <span></span>
            自动更新
          </label>
          <button
            class="primary-btn"
            :disabled="aiStore.isBusy || updatingToCurrent"
            @click="updateToCurrent"
          >
            {{ aiStore.phase === 'text' || updatingToCurrent ? '更新中...' : '更新到当前进度' }}
          </button>
          <button class="ghost-danger-btn" title="清空当前书的 AI资料" @click="resetMemory">
            重置
          </button>
        </div>
      </header>

      <div v-if="statusNotice" class="status-strip" :class="{ error: statusNotice.isError }">
        <div class="status-main">
          <strong>{{ statusNotice.isError ? '生成失败' : '状态' }}</strong>
          <p>{{ statusNotice.summary }}</p>
        </div>
        <details v-if="statusNotice.detail" class="status-detail">
          <summary>查看详情</summary>
          <pre>{{ statusNotice.detail }}</pre>
        </details>
      </div>

      <nav class="tabs">
        <button
          v-for="tab in tabs"
          :key="tab.key"
          :class="{ active: activeTab === tab.key }"
          @click="activeTab = tab.key"
        >
          {{ tab.label }}
        </button>
      </nav>

      <main class="ai-content">
        <section v-if="activeTab === 'overview'" class="overview-grid">
          <div class="overview-main">
            <h2>总览</h2>
            <section class="overview-section">
              <h3>剧情摘要</h3>
              <p class="summary">{{ memory.summary || '暂无资料' }}</p>
            </section>
            <section class="overview-section">
              <h3>世界观资料</h3>
              <div class="worldview-groups">
                <section
                  v-for="group in worldviewGroups"
                  :key="group.category"
                  class="worldview-group"
                >
                  <div class="group-head">
                    <button class="group-toggle" @click="toggleWorldviewGroup(group.category)">
                      <span>{{ group.collapsed ? '+' : '-' }}</span>
                      <h3>{{ group.category }}</h3>
                    </button>
                    <span>{{ group.items.length }}</span>
                  </div>
                  <div v-if="!group.collapsed" class="group-items">
                    <article
                      v-for="note in group.items"
                      :key="`${group.category}-${note.title}`"
                      class="note-item"
                    >
                      <div class="item-title">
                        <h4>{{ note.title }}</h4>
                        <span v-if="note.confidence">{{ note.confidence }}</span>
                      </div>
                      <p>{{ note.content }}</p>
                    </article>
                  </div>
                </section>
              </div>
              <EmptyState v-if="!worldviewGroups.length" text="暂无世界观资料" />
            </section>
          </div>

          <aside class="overview-side">
            <div class="metric">
              <span>角色</span>
              <strong>{{ importantCharacters.length }}</strong>
            </div>
            <div class="metric">
              <span>关系</span>
              <strong>{{ displayRelationships.length }}</strong>
            </div>
            <div class="metric">
              <span>地点</span>
              <strong>{{ displayLocations.length }}</strong>
            </div>
            <div class="metric">
              <span>最近章节</span>
              <strong>{{
                memory.processedChapterIndex != null ? memory.processedChapterIndex + 1 : '-'
              }}</strong>
            </div>
          </aside>
        </section>

        <section v-else-if="activeTab === 'characters'" class="list-panel">
          <div class="panel-toolbar">
            <label class="search-field">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <circle cx="11" cy="11" r="7" />
                <path d="m20 20-3.5-3.5" />
              </svg>
              <input v-model="characterSearch" placeholder="搜索角色、别名、势力、位置" />
            </label>
            <span class="result-count"
              >{{ filteredCharacters.length }} / {{ importantCharacters.length }}</span
            >
          </div>
          <article v-for="character in filteredCharacters" :key="character.name" class="list-item">
            <div class="item-title">
              <h3>{{ character.name }}</h3>
              <span v-if="character.faction">{{ character.faction }}</span>
            </div>
            <p>{{ character.status || character.description || '暂无状态' }}</p>
            <div class="meta-line">
              <span v-if="character.location">位置：{{ character.location }}</span>
              <span v-if="character.lastSeenChapter">最近：{{ character.lastSeenChapter }}</span>
              <span v-if="character.aliases?.length">别名：{{ character.aliases.join('、') }}</span>
            </div>
          </article>
          <EmptyState
            v-if="!filteredCharacters.length"
            :text="importantCharacters.length ? '没有匹配的角色' : '暂无重要角色资料'"
          />
        </section>

        <section v-else-if="activeTab === 'relationships'" class="relation-grid">
          <article
            v-for="relationship in displayRelationships"
            :key="`${relationship.source}-${relationship.target}-${relationship.relation}`"
            class="relation-item"
          >
            <div class="relation-head">
              <strong>{{ relationship.source }}</strong>
              <span>{{ relationship.relation }}</span>
              <strong>{{ relationship.target }}</strong>
            </div>
            <p>{{ relationship.description || relationship.status || '暂无说明' }}</p>
          </article>
          <EmptyState v-if="!displayRelationships.length" text="暂无重要人物关系" />
        </section>

        <section v-else-if="activeTab === 'map'" class="map-panel">
          <div class="map-toolbar">
            <div class="map-title">
              <h2>世界地图</h2>
              <p>{{ memory.map?.updatedAt ? formatDateTime(memory.map.updatedAt) : '未生成' }}</p>
            </div>
            <button class="secondary-btn" :disabled="aiStore.isBusy" @click="redrawMap">
              {{ aiStore.phase === 'map' ? '绘制中...' : '重绘地图' }}
            </button>
          </div>

          <div class="map-frame">
            <img v-if="memory.map?.imageUrl" :src="memory.map.imageUrl" alt="世界地图" />
            <RelationshipGraph
              v-else
              :memory="displayMemory"
              :fallback-reason="memory.map?.fallbackReason"
            />
          </div>

          <div class="location-tree">
            <article
              v-for="row in visibleLocationRows"
              :key="row.location.name"
              class="location-item tree-location"
              :style="{ '--depth-offset': `${row.depth * 22}px` }"
            >
              <div class="item-title">
                <div class="location-title-wrap">
                  <button
                    v-if="row.hasChildren"
                    class="location-toggle"
                    :aria-label="isLocationCollapsed(row.location.name) ? '展开地点' : '收起地点'"
                    @click="toggleLocation(row.location.name)"
                  >
                    {{ isLocationCollapsed(row.location.name) ? '+' : '-' }}
                  </button>
                  <span v-else class="location-toggle ghost"></span>
                  <h3>{{ row.location.name }}</h3>
                </div>
                <span v-if="row.location.kind">{{ row.location.kind }}</span>
              </div>
              <p>{{ row.location.description }}</p>
              <div class="meta-line">
                <span v-if="row.location.status">状态：{{ row.location.status }}</span>
                <span v-if="row.location.parentName">上级：{{ row.location.parentName }}</span>
                <span v-if="row.location.relatedCharacters?.length"
                  >相关：{{ row.location.relatedCharacters.join('、') }}</span
                >
              </div>
            </article>
            <EmptyState v-if="!visibleLocationRows.length" text="暂无地点资料" />
          </div>
        </section>
      </main>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, defineComponent, h, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { getBookContent, getChapterList, getShelfBook } from '../api/bookshelf'
import { useAiBookStore } from '../stores/aiBook'
import { useAppStore } from '../stores/app'
import { useReaderStore } from '../stores/reader'
import { formatDateTime } from '../utils/format'
import type {
  AiBookCharacter,
  AiBookLocation,
  AiBookMemory,
  AiBookRelationship,
  Book,
  BookChapter,
} from '../types'
import RelationshipGraph from '../components/ai-book/RelationshipGraph.vue'
import {
  isLowImportance,
  isLowValueRelationship,
  normalizeKey,
  preferImportance,
  relationshipKey,
  richerString,
  uniqueStrings,
} from '../utils/aiBookNormalize'
import { buildAiBookLocationRows, groupAiBookWorldview } from '../utils/aiBookPresentation'
import { collapseWhitespace, summarizeDisplayError } from '../utils/httpError'

type AiTab = 'overview' | 'characters' | 'relationships' | 'map'

const EmptyState = defineComponent({
  props: { text: { type: String, required: true } },
  setup(props) {
    return () => h('div', { class: 'empty-state' }, props.text)
  },
})

const route = useRoute()
const router = useRouter()
const aiStore = useAiBookStore()
const appStore = useAppStore()
const readerStore = useReaderStore()

const loading = ref(true)
const activeTab = ref<AiTab>('overview')
const book = ref<Book | null>(null)
const chapters = ref<BookChapter[]>([])
const characterSearch = ref('')
const collapsedLocationIds = ref(new Set<string>())
const collapsedWorldviewCategories = ref(new Set<string>())

const tabs: Array<{ key: AiTab; label: string }> = [
  { key: 'overview', label: '总览' },
  { key: 'characters', label: '角色' },
  { key: 'relationships', label: '关系' },
  { key: 'map', label: '地图' },
]

const memory = computed(() => aiStore.memory)
const worldviewGroups = computed(() =>
  groupAiBookWorldview(memory.value?.worldview || [], collapsedWorldviewCategories.value)
)
const importantCharacters = computed(() =>
  normalizeDisplayCharacters(memory.value?.characters || [])
)
const filteredCharacters = computed(() =>
  filterCharacters(importantCharacters.value, characterSearch.value)
)
const displayRelationships = computed(() =>
  normalizeDisplayRelationships(memory.value?.relationships || [])
)
const displayLocations = computed(() => normalizeDisplayLocations(memory.value?.locations || []))
const visibleLocationRows = computed(() =>
  buildAiBookLocationRows(displayLocations.value, collapsedLocationIds.value)
)
const displayMemory = computed<AiBookMemory | null>(() =>
  memory.value
    ? {
        ...memory.value,
        characters: importantCharacters.value,
        relationships: displayRelationships.value,
        locations: displayLocations.value,
      }
    : null
)
const progressText = computed(() => {
  const index = memory.value?.processedChapterIndex
  if (index == null) return '尚未生成'
  return `已更新至第 ${index + 1} 章`
})
const statusNotice = computed(() => {
  const source = aiStore.statusText || memory.value?.lastError || ''
  if (!source.trim()) return null
  const isLastError = !aiStore.statusText && Boolean(memory.value?.lastError)
  const summary = summarizeDisplayError(source)
  const normalizedSource = collapseWhitespace(source)
  const hasDetail = normalizedSource !== summary && source.trim().length > summary.length + 20
  return {
    summary,
    detail: hasDetail ? source.trim() : '',
    isError: aiStore.phase === 'error' || isLastError,
  }
})

onMounted(async () => {
  await appStore.fetchUserInfo()
  aiStore.refreshConfig()
  const bookUrl = String(route.query.bookUrl || '')
  if (!bookUrl) {
    router.replace('/')
    return
  }
  try {
    book.value = await getShelfBook(bookUrl)
    await aiStore.load(book.value)
    chapters.value = await getChapterList({
      bookUrl: book.value.bookUrl,
      bookSourceUrl: book.value.origin,
    }).catch(() => [])
  } catch (error) {
    appStore.showToast((error as Error).message || 'AI资料加载失败', 'error')
    router.replace('/')
  } finally {
    loading.value = false
  }
})

function goBack() {
  router.back()
}

async function toggleEnabled(event: Event) {
  if (!book.value) return
  const enabled = (event.target as HTMLInputElement).checked
  try {
    await aiStore.setEnabled(book.value, enabled)
    appStore.showToast(enabled ? '已开启自动更新' : '已关闭自动更新', 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || '设置失败', 'error')
  }
}

// 逐章循环里每章结束 phase 会短暂回到 idle（拉下一章正文期间），
// isBusy 会出现空窗；整个循环用一个本地守卫盖住
const updatingToCurrent = ref(false)

async function updateToCurrent() {
  if (!book.value || !memory.value || updatingToCurrent.value) return
  const targetIndex = resolveCurrentIndex()
  if (!chapters.value.length) {
    appStore.showToast('目录未加载，无法更新', 'warning')
    return
  }
  const startIndex = Math.max(0, (memory.value.processedChapterIndex ?? -1) + 1)
  if (startIndex > targetIndex) {
    appStore.showToast('当前进度已更新', 'success')
    return
  }

  updatingToCurrent.value = true
  try {
    let currentMemory = memory.value
    for (let index = startIndex; index <= targetIndex; index += 1) {
      const chapter = chapters.value[index]
      if (!chapter) continue
      const chapterContent = await resolveChapterContent(index, chapter)
      currentMemory = await aiStore.runChapterUpdate({
        book: book.value,
        chapter,
        chapterContent,
        current: currentMemory,
        chapters: chapters.value,
      })
    }
    appStore.showToast('AI资料已更新', 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || 'AI资料更新失败', 'error')
  } finally {
    updatingToCurrent.value = false
  }
}

async function redrawMap() {
  if (!book.value) return
  const next = await aiStore.redrawMap(book.value)
  if (next?.map?.imageUrl) {
    appStore.showToast('地图已更新', 'success')
  } else {
    appStore.showToast('图片地图不可用，已显示关系图', 'warning')
  }
}

function toggleLocation(name: string) {
  const key = normalizeKey(name)
  const next = new Set(collapsedLocationIds.value)
  if (next.has(key)) {
    next.delete(key)
  } else {
    next.add(key)
  }
  collapsedLocationIds.value = next
}

function isLocationCollapsed(name: string) {
  return collapsedLocationIds.value.has(normalizeKey(name))
}

function toggleWorldviewGroup(category: string) {
  const key = normalizeKey(category)
  const next = new Set(collapsedWorldviewCategories.value)
  if (next.has(key)) {
    next.delete(key)
  } else {
    next.add(key)
  }
  collapsedWorldviewCategories.value = next
}

async function resetMemory() {
  if (!book.value) return
  if (!confirm('确定重置当前书的 AI资料？')) return
  try {
    await aiStore.reset(book.value)
    appStore.showToast('AI资料已重置', 'success')
  } catch (error) {
    appStore.showToast((error as Error).message || '重置失败', 'error')
  }
}

function resolveCurrentIndex() {
  if (readerStore.book?.bookUrl === book.value?.bookUrl) {
    return Math.max(0, readerStore.currentIndex)
  }
  return Math.max(0, Math.min(chapters.value.length - 1, book.value?.durChapterIndex || 0))
}

async function resolveChapterContent(index: number, chapter: BookChapter) {
  if (readerStore.book?.bookUrl === book.value?.bookUrl) {
    const content = await readerStore.fetchChapterContent(index)
    if (content) return content
  }
  return getBookContent({
    chapterUrl: chapter.url,
    bookSourceUrl: book.value?.origin,
  })
}

function normalizeDisplayCharacters(characters: AiBookCharacter[]) {
  const byName = new Map<string, AiBookCharacter>()
  for (const character of characters) {
    if (!character.name || isLowImportance(character.importance)) continue
    const key = normalizeKey(character.name)
    const existing = byName.get(key)
    byName.set(key, existing ? mergeDisplayCharacter(existing, character) : character)
  }
  return [...byName.values()]
}

function filterCharacters(characters: AiBookCharacter[], query: string) {
  const normalizedQuery = normalizeSearch(query)
  if (!normalizedQuery) return characters
  return characters.filter((character) =>
    normalizeSearch(
      [
        character.name,
        character.aliases?.join(' '),
        character.status,
        character.faction,
        character.location,
        character.description,
      ]
        .filter(Boolean)
        .join(' ')
    ).includes(normalizedQuery)
  )
}

function normalizeDisplayRelationships(relationships: AiBookRelationship[]) {
  const byPair = new Map<string, AiBookRelationship>()
  for (const relationship of relationships) {
    if (
      !relationship.source ||
      !relationship.target ||
      !relationship.relation ||
      normalizeKey(relationship.source) === normalizeKey(relationship.target) ||
      isLowImportance(relationship.importance) ||
      isLowValueRelationship(
        relationship.relation,
        relationship.description || relationship.status || '',
        relationship.importance
      )
    ) {
      continue
    }
    const key = relationshipKey(relationship.source, relationship.target, relationship.relation)
    const existing = byPair.get(key)
    byPair.set(key, existing ? mergeDisplayRelationship(existing, relationship) : relationship)
  }
  return [...byPair.values()]
}

function normalizeDisplayLocations(locations: AiBookLocation[]) {
  const byName = new Map<string, AiBookLocation>()
  for (const location of locations) {
    if (!location.name || isLowImportance(location.importance)) continue
    const parentName =
      location.parentName && normalizeKey(location.parentName) !== normalizeKey(location.name)
        ? location.parentName
        : undefined
    const normalized = { ...location, parentName }
    const key = normalizeKey(location.name)
    const existing = byName.get(key)
    byName.set(key, existing ? mergeDisplayLocation(existing, normalized) : normalized)
  }
  return [...byName.values()]
}

function mergeDisplayCharacter(current: AiBookCharacter, next: AiBookCharacter): AiBookCharacter {
  return {
    ...current,
    aliases: uniqueStrings([...(current.aliases || []), ...(next.aliases || [])]),
    status: richerString(current.status, next.status),
    faction: current.faction || next.faction,
    location: current.location || next.location,
    description: richerString(current.description, next.description),
    lastSeenChapter: current.lastSeenChapter || next.lastSeenChapter,
    importance: preferImportance(current.importance, next.importance),
  }
}

function mergeDisplayRelationship(
  current: AiBookRelationship,
  next: AiBookRelationship
): AiBookRelationship {
  return {
    ...current,
    status: richerString(current.status, next.status),
    description: richerString(current.description, next.description),
    importance: preferImportance(current.importance, next.importance),
  }
}

function mergeDisplayLocation(current: AiBookLocation, next: AiBookLocation): AiBookLocation {
  return {
    ...current,
    kind: current.kind || next.kind,
    parentName: current.parentName || next.parentName,
    description: richerString(current.description, next.description),
    status: richerString(current.status, next.status),
    relatedCharacters: uniqueStrings([
      ...(current.relatedCharacters || []),
      ...(next.relatedCharacters || []),
    ]),
    firstSeenChapter: current.firstSeenChapter || next.firstSeenChapter,
    importance: preferImportance(current.importance, next.importance),
  }
}

function normalizeSearch(value: string) {
  return value.trim().toLowerCase().replace(/\s+/g, '')
}
</script>

<style scoped>
.ai-book-view {
  height: 100%;
  overflow: hidden;
  background: var(--color-bg);
  color: var(--color-text);
}

.ai-loading {
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
  color: var(--color-text-secondary);
}

.ai-shell {
  height: 100%;
  display: flex;
  flex-direction: column;
  max-width: 1240px;
  margin: 0 auto;
  padding: 16px 28px 22px;
  box-sizing: border-box;
  min-height: 0;
}

.ai-header {
  display: flex;
  justify-content: space-between;
  gap: 18px;
  align-items: center;
  border-bottom: 1px solid var(--color-border-light);
  padding-bottom: 12px;
}

.title-stack {
  min-width: 0;
}

.title-row {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.ai-header h1 {
  margin: 0;
  min-width: 0;
  font-size: var(--text-xl);
  line-height: 1.2;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.ai-header p,
.map-toolbar p {
  margin: 4px 0 0;
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
}

.back-btn,
.secondary-btn,
.primary-btn,
.ghost-danger-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  min-height: 34px;
  padding: 0 13px;
  border-radius: 8px;
  border: 1px solid var(--color-border);
  color: var(--color-text-secondary);
  background: var(--color-bg-elevated);
  font-weight: 600;
  cursor: pointer;
}

.back-btn svg {
  width: 16px;
  height: 16px;
}

.back-btn {
  flex: 0 0 auto;
  background: transparent;
}

.primary-btn {
  background: var(--color-primary);
  border-color: var(--color-primary);
  color: #fff;
}

.ghost-danger-btn {
  color: var(--color-danger, #d14b4b);
  background: transparent;
  border-color: transparent;
}

.ghost-danger-btn:hover {
  background: rgba(209, 75, 75, 0.1);
}

.primary-btn:disabled,
.secondary-btn:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}

.header-actions,
.map-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
}

.enable-switch {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: var(--text-sm);
  color: var(--color-text-secondary);
  cursor: pointer;
}

.enable-switch input {
  display: none;
}

.enable-switch span {
  width: 36px;
  height: 20px;
  border-radius: 999px;
  background: var(--color-border);
  position: relative;
  transition: background var(--duration-fast);
}

.enable-switch span::after {
  content: '';
  position: absolute;
  width: 16px;
  height: 16px;
  left: 2px;
  top: 2px;
  border-radius: 50%;
  background: #fff;
  transition: transform var(--duration-fast);
}

.enable-switch input:checked + span {
  background: var(--color-primary);
}

.enable-switch input:checked + span::after {
  transform: translateX(16px);
}

.status-strip {
  margin-top: 14px;
  padding: 10px 12px;
  border-radius: 8px;
  background: rgba(201, 127, 58, 0.12);
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
  flex: 0 0 auto;
  max-height: 240px;
  overflow: hidden;
}

.status-strip.error {
  background: rgba(209, 75, 75, 0.12);
}

.status-main {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  min-width: 0;
}

.status-main strong {
  flex: 0 0 auto;
  color: var(--color-text);
  font-weight: 700;
}

.status-main p {
  min-width: 0;
  margin: 0;
  line-height: 1.55;
  overflow: hidden;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
}

.status-detail {
  margin-top: 8px;
}

.status-detail summary {
  width: fit-content;
  cursor: pointer;
  color: var(--color-primary);
  font-weight: 700;
}

.status-detail pre {
  max-height: 150px;
  margin: 8px 0 0;
  padding: 10px;
  overflow: auto;
  border-radius: 6px;
  border: 1px solid rgba(209, 75, 75, 0.18);
  background: rgba(255, 255, 255, 0.46);
  white-space: pre-wrap;
  word-break: break-word;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: var(--text-xs);
  line-height: 1.5;
}

.tabs {
  display: flex;
  gap: 4px;
  margin-top: 10px;
  border-bottom: 1px solid var(--color-border-light);
}

.tabs button {
  padding: 10px 15px;
  color: var(--color-text-tertiary);
  font-weight: 600;
  border-bottom: 2px solid transparent;
}

.tabs button.active {
  color: var(--color-primary);
  border-color: var(--color-primary);
}

.ai-content {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 14px 0 28px;
  scrollbar-width: none;
}

.overview-grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 260px;
  gap: 24px;
}

.overview-main h2 {
  margin: 0 0 14px;
  font-size: var(--text-lg);
}

.overview-section + .overview-section {
  margin-top: 20px;
}

.overview-section > h3 {
  margin: 0 0 10px;
  font-size: var(--text-base);
}

.summary {
  margin: 0;
  line-height: 1.8;
  color: var(--color-text-secondary);
}

.worldview-groups,
.worldview-group,
.list-panel,
.relation-grid,
.location-tree {
  display: grid;
  gap: 12px;
}

.worldview-group {
  gap: 10px;
}

.group-items {
  display: grid;
  gap: 10px;
}

.group-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 0 2px;
}

.group-toggle {
  min-width: 0;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  color: var(--color-text);
  font-weight: 700;
  text-align: left;
}

.group-toggle span {
  width: 22px;
  height: 22px;
  flex: 0 0 22px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--color-border);
  border-radius: 6px;
  background: var(--color-bg-elevated);
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
  line-height: 1;
}

.group-head h3 {
  margin: 0;
  font-size: var(--text-base);
}

.group-head span,
.result-count {
  color: var(--color-text-tertiary);
  font-size: var(--text-xs);
  font-weight: 600;
}

.note-item,
.list-item,
.relation-item,
.location-item,
.metric {
  border: 1px solid var(--color-border-light);
  border-radius: 8px;
  padding: 14px;
  background: var(--color-bg-elevated);
}

.item-title,
.relation-head {
  display: flex;
  align-items: center;
  gap: 10px;
  justify-content: space-between;
}

.item-title h3 {
  margin: 0;
  font-size: var(--text-base);
}

.item-title h4 {
  margin: 0;
  font-size: var(--text-base);
}

.item-title span,
.relation-head span {
  font-size: var(--text-xs);
  color: var(--color-primary);
}

.note-item p,
.list-item p,
.relation-item p,
.location-item p {
  margin: 8px 0 0;
  line-height: 1.7;
  color: var(--color-text-secondary);
}

.overview-side {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  align-content: start;
  gap: 12px;
}

.metric {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.metric span,
.meta-line {
  color: var(--color-text-tertiary);
  font-size: var(--text-xs);
}

.metric strong {
  font-size: var(--text-xl);
}

.meta-line {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  margin-top: 10px;
}

.panel-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  position: sticky;
  top: -14px;
  z-index: 2;
  padding: 2px 0 10px;
  background: var(--color-bg);
}

.search-field {
  min-width: 260px;
  max-width: 420px;
  flex: 1;
  display: flex;
  align-items: center;
  gap: 8px;
  min-height: 38px;
  padding: 0 12px;
  border: 1px solid var(--color-border-light);
  border-radius: 8px;
  background: var(--color-bg-elevated);
  color: var(--color-text-tertiary);
}

.search-field svg {
  width: 16px;
  height: 16px;
  flex: 0 0 auto;
}

.search-field input {
  min-width: 0;
  flex: 1;
  border: 0;
  outline: 0;
  background: transparent;
  color: var(--color-text);
  font-size: var(--text-base);
}

.relation-grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.map-panel {
  display: grid;
  gap: 12px;
}

.map-toolbar {
  justify-content: space-between;
  min-height: 42px;
  padding: 0 2px;
}

.map-title {
  display: flex;
  align-items: baseline;
  gap: 10px;
  min-width: 0;
}

.map-title h2 {
  margin: 0;
  font-size: var(--text-lg);
  white-space: nowrap;
}

.map-title p {
  margin: 0;
  white-space: nowrap;
}

.map-frame {
  min-height: min(58vh, 640px);
  border-radius: 8px;
  overflow: hidden;
  border: 1px solid var(--color-border-light);
  background: #1f2522;
  display: flex;
  align-items: center;
  justify-content: center;
}

.map-frame img {
  width: 100%;
  height: 100%;
  max-height: 620px;
  object-fit: contain;
  display: block;
}

.location-tree {
  gap: 10px;
}

.tree-location {
  margin-left: var(--depth-offset);
  position: relative;
}

.tree-location::before {
  content: '';
  position: absolute;
  left: -12px;
  top: -10px;
  bottom: -10px;
  width: 1px;
  background: var(--color-border-light);
  opacity: 0.55;
}

.tree-location[style*='--depth-offset: 0px']::before {
  display: none;
}

.location-title-wrap {
  min-width: 0;
  display: inline-flex;
  align-items: center;
  gap: 8px;
}

.location-toggle {
  width: 22px;
  height: 22px;
  flex: 0 0 22px;
  border: 1px solid var(--color-border);
  border-radius: 6px;
  background: var(--color-bg);
  color: var(--color-text-secondary);
  font-weight: 700;
  line-height: 1;
}

.location-toggle.ghost {
  border-color: transparent;
  background: transparent;
}

.empty-state {
  color: var(--color-text-tertiary);
  padding: 48px;
  text-align: center;
}

@media (max-width: 767px) {
  .ai-shell {
    padding: 16px;
  }

  .ai-header,
  .header-actions,
  .map-toolbar,
  .panel-toolbar {
    align-items: stretch;
    flex-direction: column;
  }

  .search-field {
    min-width: 0;
    max-width: none;
  }

  .title-row {
    align-items: flex-start;
    flex-direction: column;
    gap: 8px;
  }

  .overview-grid,
  .relation-grid {
    grid-template-columns: 1fr;
  }

  .tabs {
    overflow-x: auto;
  }

  .tabs button {
    flex: 0 0 auto;
  }
}
</style>
