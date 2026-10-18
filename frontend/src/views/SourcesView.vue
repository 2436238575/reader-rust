<template>
  <div class="sources-view">
    <SourceManagerHeader
      :total="sourceStats.total"
      :enabled="sourceStats.enabled"
      :filtered="sourceStats.filtered"
      :selected="selectedFilteredSources.length"
      :loading="loading"
      :testing="testingSources"
      :invalid-count="invalidSources.length"
      @refresh="loadSources"
      @import-local="triggerFileImport"
      @open-subscriptions="subscriptionPanelVisible = true"
      @export="exportSources"
      @test-sources="testSources"
      @delete-invalid="removeInvalidSources"
      @create="createSource"
      @back="goBack"
    />

    <input
      ref="fileInputRef"
      type="file"
      accept=".json,.txt"
      class="hidden-input"
      @change="handleFileImport"
    />

    <SourceFilterBar
      :filter-text="filterText"
      :filter-group="filterGroup"
      :groups="groupList"
      :all-selected="allFilteredSelected"
      :partially-selected="partiallyFilteredSelected"
      :selected-count="selectedFilteredSources.length"
      :has-sources="filteredSources.length > 0"
      @update:filter-text="filterText = $event"
      @update:filter-group="filterGroup = $event"
      @toggle-current-selection="toggleFilteredSelection"
      @clear-selection="clearSelection"
      @delete-selection="removeSelectedSources"
    />

    <div class="content-grid">
      <SourceList
        class="source-list-slot"
        :sources="filteredSources"
        :loading="loading"
        :selected-ids="selectedSourceIds"
        :active-id="editingSource?.bookSourceId"
        :empty-title="sources.length ? '没有匹配的书源' : '暂无书源'"
        :empty-description="
          sources.length ? '调整搜索关键词或分组筛选后再试' : '可以本地导入、远程同步或手动新增'
        "
        @edit="editSource"
        @toggle-enabled="toggleSource"
        @toggle-selection="toggleSourceSelection"
        @delete="removeSource"
      />

      <SourceEditorPanel
        class="editor-slot"
        :source="editingSource"
        :editor-text="editorText"
        :can-login="canLoginSource"
        :login-loading="sourceLoginLoading"
        @update:editor-text="editorText = $event"
        @format="formatEditor"
        @save="saveEditor"
        @login="handleSourceLogin"
        @create="createSource"
        @import-local="triggerFileImport"
      />
    </div>

    <footer class="page-footer">
      <span class="count-info">显示 {{ filteredSources.length }} / {{ sources.length }}</span>
    </footer>

    <SourceSubscriptionPanel
      v-model:remote-url="remoteUrl"
      :visible="subscriptionPanelVisible"
      :subscriptions="subscriptions"
      @sync="importRemoteSource"
      @save="saveSubscription"
      @sync-subscription="syncSubscription"
      @remove-subscription="removeSubscription"
      @close="subscriptionPanelVisible = false"
    />

    <Transition name="scale">
      <div
        v-if="loginPreviewVisible"
        class="login-preview-container"
        @click.self="loginPreviewVisible = false"
      >
        <div class="login-preview-modal">
          <div class="login-preview-header">
            <div>
              <h3>书源登录页</h3>
              <p>{{ loginPreviewUrl }}</p>
            </div>
            <button
              class="icon-btn"
              type="button"
              @click="loginPreviewVisible = false"
              title="关闭"
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
            </button>
          </div>
          <!-- 被代理的是第三方书源页面：sandbox 把它关进不透明源，
               防它的脚本读主应用 localStorage / DOM；页面内请求靠后端
               签发的限定令牌（绑定书源、短寿命、只能走代理路径） -->
          <iframe
            class="login-preview-frame"
            :src="loginPreviewFrameUrl"
            sandbox="allow-scripts allow-forms"
          ></iframe>
        </div>
      </div>
    </Transition>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { onBeforeRouteLeave, useRouter } from 'vue-router'
import { API_BASE } from '../utils/appBase'
import {
  getBookSources,
  deleteBookSource,
  deleteBookSources,
  deleteInvalidBookSources,
  loginBookSource,
  saveBookSource,
  saveBookSources,
  testBookSources,
  readRemoteSourceFile,
  readSourceFile,
} from '../api/source'
import { useAppStore } from '../stores/app'
import type { BookSource } from '../types'
import {
  filterBookSources,
  getBookSourceGroups,
  getBookSourceStats,
  splitBookSourceGroups,
} from '../utils/sourceSelection'
import { appendAuthQueryParams } from '../utils/secureAccess'
import { MAX_SOURCE_TEST_BATCH_SIZE, mergeBookSourceTestResponses } from '../utils/sourceTesting'
import SourceEditorPanel from '../components/source-manager/SourceEditorPanel.vue'
import SourceFilterBar from '../components/source-manager/SourceFilterBar.vue'
import SourceList from '../components/source-manager/SourceList.vue'
import SourceManagerHeader from '../components/source-manager/SourceManagerHeader.vue'
import SourceSubscriptionPanel from '../components/source-manager/SourceSubscriptionPanel.vue'

type SourceSubscription = {
  url: string
  lastSyncedAt?: number
}

const SUBSCRIPTION_KEY = 'reader-source-subscriptions'

const appStore = useAppStore()
const router = useRouter()

const sources = ref<BookSource[]>([])
const loading = ref(false)
const testingSources = ref(false)
const filterText = ref('')
const filterGroup = ref('')
const fileInputRef = ref<HTMLInputElement | null>(null)
// 书源身份是后端分配的 bookSourceId（number）；bookSourceUrl 只是展示数据
const selectedSourceIds = ref(new Set<number>())

const subscriptionPanelVisible = ref(false)
const remoteUrl = ref('')
const subscriptions = ref<SourceSubscription[]>(loadSubscriptions())

const editingSource = ref<BookSource | null>(null)
const editorText = ref(JSON.stringify(createEmptySource(), null, 2))
// 最近一次落盘（或载入编辑器）时的文本：与 editorText 不一致即视为未保存
const lastSavedEditorText = ref(editorText.value)
const sourceLoginLoading = ref(false)
const loginPreviewVisible = ref(false)
const loginPreviewUrl = ref('')
// 保存/导入/同步都改书源并整表重载：串行化，挡住连点产生的并发写
const sourceIoBusy = ref(false)
const loginPreviewFrameUrl = ref('')

const groupList = computed(() => getBookSourceGroups(sources.value))

const filteredSources = computed(() =>
  filterBookSources(sources.value, filterText.value, filterGroup.value)
)

const sourceStats = computed(() => getBookSourceStats(sources.value, filteredSources.value))

const selectedFilteredSources = computed(() =>
  filteredSources.value.filter(
    (source): source is BookSource & { bookSourceId: number } =>
      source.bookSourceId != null && selectedSourceIds.value.has(source.bookSourceId)
  )
)

const invalidSources = computed(() =>
  sources.value.filter((source) => splitBookSourceGroups(source.bookSourceGroup).includes('失效'))
)

const allFilteredSelected = computed(
  () =>
    filteredSources.value.length > 0 &&
    selectedFilteredSources.value.length === filteredSources.value.length
)

const partiallyFilteredSelected = computed(
  () => selectedFilteredSources.value.length > 0 && !allFilteredSelected.value
)

const canLoginSource = computed(() => {
  try {
    const parsed = JSON.parse(editorText.value) as BookSource
    return Boolean(parsed.bookSourceUrl?.trim() && parsed.loginUrl?.trim())
  } catch {
    return false
  }
})

const editorDirty = computed(() => editorText.value !== lastSavedEditorText.value)

function createEmptySource(): BookSource {
  return {
    bookSourceName: '新增书源',
    bookSourceUrl: '',
    enabled: true,
  }
}

function loadSubscriptions(): SourceSubscription[] {
  try {
    const raw = localStorage.getItem(SUBSCRIPTION_KEY)
    return raw ? JSON.parse(raw) : []
  } catch {
    return []
  }
}

function persistSubscriptions() {
  localStorage.setItem(SUBSCRIPTION_KEY, JSON.stringify(subscriptions.value))
}

async function loadSources() {
  loading.value = true
  try {
    sources.value = await getBookSources()
    pruneSelection()
  } catch (e: unknown) {
    appStore.showToast((e as Error).message, 'error')
  } finally {
    loading.value = false
  }
}

async function toggleSource(source: BookSource) {
  const next = { ...source, enabled: source.enabled === false ? true : false }
  try {
    await saveBookSource(next)
    Object.assign(source, next)
    appStore.showToast('书源状态已更新', 'success')
  } catch (e: unknown) {
    appStore.showToast((e as Error).message, 'error')
  }
}

async function removeSource(source: BookSource) {
  if (source.bookSourceId == null) return
  if (!confirm(`确定删除书源 "${source.bookSourceName}"？`)) return
  try {
    await deleteBookSource(source.bookSourceId)
    sources.value = sources.value.filter((s) => s.bookSourceId !== source.bookSourceId)
    selectedSourceIds.value.delete(source.bookSourceId)
    if (editingSource.value?.bookSourceId === source.bookSourceId) {
      createSource()
    }
    appStore.showToast('已删除', 'success')
  } catch (e: unknown) {
    appStore.showToast((e as Error).message, 'error')
  }
}

async function removeSelectedSources() {
  const targets = selectedFilteredSources.value
  if (!targets.length) {
    appStore.showToast('请先选择要删除的书源', 'warning')
    return
  }
  if (!confirm(`确定删除选中的 ${targets.length} 个书源？`)) return
  try {
    await deleteBookSources(targets.map((source) => ({ bookSourceId: source.bookSourceId })))
    const targetIds = new Set(targets.map((source) => source.bookSourceId))
    sources.value = sources.value.filter(
      (source) => source.bookSourceId == null || !targetIds.has(source.bookSourceId)
    )
    selectedSourceIds.value.clear()
    if (
      editingSource.value?.bookSourceId != null &&
      targetIds.has(editingSource.value.bookSourceId)
    ) {
      createSource()
    }
    appStore.showToast(`已删除 ${targets.length} 个书源`, 'success')
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '批量删除失败', 'error')
  }
}

async function testSources() {
  const targets = selectedFilteredSources.value.length
    ? selectedFilteredSources.value
    : sources.value
  if (!targets.length) {
    appStore.showToast('没有可测试的书源', 'warning')
    return
  }
  const scopeText = selectedFilteredSources.value.length
    ? `选中的 ${targets.length}`
    : `全部 ${targets.length}`
  if (!confirm(`将测试${scopeText} 个书源。测试会请求外部站点，耗时可能较长，是否继续？`)) return

  testingSources.value = true
  try {
    const ids = targets
      .map((source) => source.bookSourceId)
      .filter((id): id is number => id != null)
    const responses = []
    for (let index = 0; index < ids.length; index += MAX_SOURCE_TEST_BATCH_SIZE) {
      responses.push(
        await testBookSources({
          bookSourceIds: ids.slice(index, index + MAX_SOURCE_TEST_BATCH_SIZE),
          markInvalid: true,
          concurrent: 12,
        })
      )
    }
    const result = mergeBookSourceTestResponses(responses)
    await loadSources()
    if (result.invalid > 0) {
      filterGroup.value = '失效'
    }
    appStore.showToast(
      `测试完成：有效 ${result.valid} 个，失效 ${result.invalid} 个，更新分组 ${result.markedInvalid} 个`,
      result.invalid > 0 ? 'warning' : 'success'
    )
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '书源测试失败', 'error')
  } finally {
    testingSources.value = false
  }
}

async function removeInvalidSources() {
  if (!invalidSources.value.length) {
    appStore.showToast('没有失效书源', 'warning')
    return
  }
  if (!confirm(`确定删除 ${invalidSources.value.length} 个失效书源？此操作不可撤销。`)) return
  try {
    const result = await deleteInvalidBookSources()
    const invalidIds = new Set(
      invalidSources.value
        .map((source) => source.bookSourceId)
        .filter((id): id is number => id != null)
    )
    sources.value = sources.value.filter(
      (source) => source.bookSourceId == null || !invalidIds.has(source.bookSourceId)
    )
    selectedSourceIds.value.clear()
    if (
      editingSource.value?.bookSourceId != null &&
      invalidIds.has(editingSource.value.bookSourceId)
    ) {
      createSource()
    }
    appStore.showToast(`已删除 ${result.deleted} 个失效书源`, 'success')
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '删除失效书源失败', 'error')
  }
}

function toggleSourceSelection(source: BookSource) {
  if (source.bookSourceId == null) return
  const selected = selectedSourceIds.value
  if (selected.has(source.bookSourceId)) {
    selected.delete(source.bookSourceId)
  } else {
    selected.add(source.bookSourceId)
  }
}

function toggleFilteredSelection() {
  const selected = selectedSourceIds.value
  if (allFilteredSelected.value) {
    filteredSources.value.forEach((source) => {
      if (source.bookSourceId != null) selected.delete(source.bookSourceId)
    })
    return
  }
  filteredSources.value.forEach((source) => {
    if (source.bookSourceId != null) selected.add(source.bookSourceId)
  })
}

function clearSelection() {
  selectedSourceIds.value.clear()
}

function pruneSelection() {
  const availableIds = new Set(
    sources.value.map((source) => source.bookSourceId).filter((id): id is number => id != null)
  )
  Array.from(selectedSourceIds.value).forEach((id) => {
    if (!availableIds.has(id)) {
      selectedSourceIds.value.delete(id)
    }
  })
}

function createSource() {
  editingSource.value = null
  editorText.value = JSON.stringify(createEmptySource(), null, 2)
  lastSavedEditorText.value = editorText.value
}

function editSource(source: BookSource) {
  editingSource.value = source
  editorText.value = JSON.stringify(source, null, 2)
  lastSavedEditorText.value = editorText.value
}

function formatEditor() {
  try {
    const parsed = JSON.parse(editorText.value)
    editorText.value = JSON.stringify(parsed, null, 2)
  } catch {
    appStore.showToast('JSON 格式错误，无法格式化', 'error')
  }
}

async function saveEditor() {
  if (sourceIoBusy.value) return
  sourceIoBusy.value = true
  try {
    const parsed = JSON.parse(editorText.value) as BookSource
    if (!parsed.bookSourceName?.trim()) {
      throw new Error('书源名称不能为空')
    }
    if (!parsed.bookSourceUrl?.trim()) {
      throw new Error('书源链接不能为空')
    }
    const saved = await saveBookSource(parsed)
    appStore.showToast('保存书源成功', 'success')
    await loadSources()
    // 新建书源保存后才有身份：以返回的 bookSourceId 匹配回填本地列表
    const latest = sources.value.find((item) => item.bookSourceId === saved.bookSourceId)
    if (latest) {
      editSource(latest)
    } else {
      lastSavedEditorText.value = editorText.value
    }
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '保存失败', 'error')
  } finally {
    sourceIoBusy.value = false
  }
}

async function handleSourceLogin() {
  try {
    const parsed = JSON.parse(editorText.value) as BookSource
    if (!parsed.bookSourceUrl?.trim()) {
      throw new Error('书源链接不能为空')
    }
    if (!parsed.loginUrl?.trim()) {
      throw new Error('当前书源未配置 loginUrl')
    }

    sourceLoginLoading.value = true
    // 编辑器内容相对已加载书源有改动（或新建）时先落盘，登录走保存后返回的 id
    let sourceId = parsed.bookSourceId ?? null
    if (!editingSource.value || editingSource.value.bookSourceUrl !== parsed.bookSourceUrl) {
      const saved = await saveBookSource(parsed)
      sourceId = saved.bookSourceId
      await loadSources()
      const latest = sources.value.find((item) => item.bookSourceId === saved.bookSourceId)
      if (latest) {
        editSource(latest)
      }
    }
    if (sourceId == null) {
      throw new Error('书源尚未保存，无法登录')
    }

    const result = await loginBookSource(sourceId)
    const check =
      typeof result.checkResult === 'string' && result.checkResult.trim()
        ? `，校验结果：${result.checkResult}`
        : ''
    if (result.url?.trim()) {
      loginPreviewUrl.value = result.url
      loginPreviewFrameUrl.value = buildLoginProxyUrl(sourceId, result.url)
      loginPreviewVisible.value = true
    }
    appStore.showToast(`书源登录请求已完成，状态 ${result.status}${check}`, 'success')
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '书源登录失败', 'error')
  } finally {
    sourceLoginLoading.value = false
  }
}

function buildLoginProxyUrl(bookSourceId: number, targetUrl: string) {
  const params = new URLSearchParams()
  appendAuthQueryParams(params)
  params.set('bookSourceId', String(bookSourceId))
  params.set('url', targetUrl)
  return `${API_BASE}/bookSourceProxy?${params.toString()}`
}

function triggerFileImport() {
  fileInputRef.value?.click()
}

async function handleFileImport(event: Event) {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  if (!file) return
  if (sourceIoBusy.value) return
  sourceIoBusy.value = true
  try {
    const imported = await readSourceFile(file)
    if (!imported.length) {
      throw new Error('文件中没有可导入的书源')
    }
    await saveBookSources(imported)
    appStore.showToast(`成功导入 ${imported.length} 个书源`, 'success')
    await loadSources()
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '导入失败', 'error')
  } finally {
    input.value = ''
    sourceIoBusy.value = false
  }
}

async function importRemoteSource() {
  if (!remoteUrl.value) {
    appStore.showToast('请输入远程书源链接', 'warning')
    return
  }
  if (sourceIoBusy.value) return
  sourceIoBusy.value = true
  try {
    const raw = await readRemoteSourceFile(remoteUrl.value)
    const parsed = raw.flatMap((item) => {
      try {
        const value = JSON.parse(item)
        return Array.isArray(value) ? value : [value]
      } catch {
        return []
      }
    }) as BookSource[]
    if (!parsed.length) {
      throw new Error('远程书源文件为空或格式错误')
    }
    await saveBookSources(parsed)
    touchSubscription(remoteUrl.value)
    appStore.showToast(`成功同步 ${parsed.length} 个书源`, 'success')
    await loadSources()
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '远程同步失败', 'error')
  } finally {
    sourceIoBusy.value = false
  }
}

function saveSubscription() {
  if (!remoteUrl.value) {
    appStore.showToast('请输入远程书源链接', 'warning')
    return
  }
  if (!subscriptions.value.find((item) => item.url === remoteUrl.value)) {
    subscriptions.value.unshift({ url: remoteUrl.value })
    persistSubscriptions()
    appStore.showToast('订阅已保存', 'success')
  }
}

async function syncSubscription(url: string) {
  remoteUrl.value = url
  await importRemoteSource()
}

function removeSubscription(url: string) {
  subscriptions.value = subscriptions.value.filter((item) => item.url !== url)
  persistSubscriptions()
}

function touchSubscription(url: string) {
  const existing = subscriptions.value.find((item) => item.url === url)
  if (existing) {
    existing.lastSyncedAt = Date.now()
  } else {
    subscriptions.value.unshift({ url, lastSyncedAt: Date.now() })
  }
  persistSubscriptions()
}

function exportSources() {
  const blob = new Blob([JSON.stringify(sources.value, null, 2)], {
    type: 'application/json;charset=utf-8',
  })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = `reader-book-sources-${formatDateForFile()}.json`
  anchor.click()
  URL.revokeObjectURL(url)
}

function formatDateForFile() {
  const date = new Date()
  const pad = (v: number) => `${v}`.padStart(2, '0')
  return `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`
}

// 深链接直达时 history 里没有上一条记录，回退落到书架页
function goBack() {
  if (window.history.state?.back) {
    router.back()
  } else {
    router.push({ name: 'home' })
  }
}

// 未保存的编辑：路由离开前 confirm 拦截（含 goBack 与浏览器前进后退）
onBeforeRouteLeave(() => {
  if (!editorDirty.value) return true
  return window.confirm('当前书源编辑内容尚未保存，确定要离开吗？')
})

// 刷新/关标签页由 beforeunload 兜底（原生提示，文案由浏览器决定）
function handleBeforeUnload(e: BeforeUnloadEvent) {
  if (editorDirty.value) e.preventDefault()
}

onMounted(() => {
  window.addEventListener('beforeunload', handleBeforeUnload)
  // 每次进入都重拉：书源可能在别处变动过（阅读器内换源/订阅同步）
  loadSources()
})

onUnmounted(() => {
  window.removeEventListener('beforeunload', handleBeforeUnload)
})
</script>

<style scoped>
.sources-view {
  height: 100%;
  min-height: 0;
  width: 100%;
  max-width: 1280px;
  margin: 0 auto;
  padding: 0 var(--space-6);
  display: flex;
  flex-direction: column;
}

.hidden-input {
  display: none;
}

.content-grid {
  display: grid;
  grid-template-columns: minmax(320px, 42%) minmax(420px, 58%);
  gap: 14px;
  min-height: 0;
  flex: 1;
  padding: 14px 0 0;
  overflow: hidden;
}

.source-list-slot,
.editor-slot {
  min-height: 0;
}

.page-footer {
  min-height: 44px;
  display: flex;
  align-items: center;
  justify-content: flex-start;
  gap: 12px;
  padding: 8px 0 calc(14px + var(--safe-area-bottom));
  flex-shrink: 0;
}

.count-info {
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.icon-btn {
  width: 36px;
  height: 36px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--color-text-secondary);
}

.icon-btn svg {
  width: 18px;
  height: 18px;
}

.icon-btn:hover {
  background: var(--color-bg-hover);
}

.login-preview-container {
  position: fixed;
  inset: 0;
  z-index: calc(var(--z-modal) + 2);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  background: var(--overlay-mask-bg);
  -webkit-backdrop-filter: var(--overlay-mask-blur);
  backdrop-filter: var(--overlay-mask-blur);
}

.login-preview-modal {
  width: min(980px, 100%);
  height: min(86vh, 900px);
  background: var(--color-bg-elevated);
  border-radius: var(--radius-xl);
  overflow: hidden;
  display: flex;
  flex-direction: column;
  box-shadow: var(--shadow-xl);
}

.login-preview-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 16px 20px;
  border-bottom: 1px solid var(--color-border-light);
}

.login-preview-header h3 {
  margin: 0;
  font-size: var(--text-lg);
  font-weight: 700;
}

.login-preview-header p {
  margin-top: 6px;
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
  word-break: break-all;
}

.login-preview-frame {
  flex: 1;
  width: 100%;
  border: none;
  background: #fff;
}

@media (max-width: 900px) {
  .sources-view {
    overflow-y: auto;
    -webkit-overflow-scrolling: touch;
    padding: 0 var(--space-4);
  }

  .content-grid {
    display: flex;
    flex-direction: column;
    overflow: visible;
    flex: none;
  }

  .editor-slot {
    order: -1;
    min-height: 360px;
    max-height: min(58vh, 560px);
  }

  .source-list-slot {
    max-height: min(46vh, 460px);
  }

  .page-footer {
    padding-bottom: calc(14px + var(--safe-area-bottom));
  }
}
</style>
