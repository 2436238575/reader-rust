<template>
  <div class="webdav-view">
    <header class="page-header">
      <button class="icon-btn" @click="goBack" aria-label="返回" title="返回">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M19 12H5" />
          <path d="M12 19l-7-7 7-7" />
        </svg>
      </button>
      <div class="title-block">
        <h2>服务器备份与文件管理</h2>
        <p class="subtitle">将书架、书源、书签、净化规则和本地阅读配置备份到服务器</p>
      </div>
    </header>

    <div class="toolbar">
      <div class="toolbar-left">
        <button class="btn btn-primary" :disabled="working" @click="createBackup">备份</button>
        <button class="btn" :disabled="working || loading" @click="loadFiles(currentPath)">
          刷新
        </button>
        <button class="btn" :disabled="working" @click="triggerUpload">上传</button>
        <input
          ref="fileInputRef"
          type="file"
          multiple
          class="hidden-input"
          @change="handleUpload"
        />
      </div>
      <button
        class="btn btn-danger"
        :disabled="working || selectedPaths.length === 0"
        @click="removeSelected"
      >
        删除选中项
      </button>
    </div>

    <div class="path-bar">
      <span class="path-label">当前目录</span>
      <code>{{ currentPath }}</code>
    </div>

    <div v-if="errorMessage" class="notice error">
      <strong>加载失败</strong>
      <span>{{ errorMessage }}</span>
    </div>

    <div class="file-list">
      <div v-if="loading" class="empty-state">正在加载文件列表...</div>
      <div v-else-if="entries.length === 0" class="empty-state">当前目录为空</div>
      <div v-else v-for="entry in entries" :key="entry.path" class="file-row">
        <label class="file-check" v-if="!entry.toParent">
          <input
            type="checkbox"
            :checked="selectedPaths.includes(entry.path)"
            @change="toggleSelection(entry.path)"
          />
        </label>
        <span v-else class="file-check placeholder"></span>

        <button class="file-main" :class="{ directory: entry.isDirectory }" @click="openEntry(entry)">
          <span class="file-icon">{{ entry.isDirectory ? '📁' : '📄' }}</span>
          <span class="file-name">{{ entry.name }}</span>
        </button>

        <span class="file-meta">{{ entry.isDirectory ? '目录' : formatBytes(entry.size) }}</span>
        <span class="file-meta">{{ formatDateTime(entry.lastModified) }}</span>

        <div class="file-actions">
          <button
            v-if="!entry.isDirectory && isBackupFile(entry.name)"
            class="btn btn-sm"
            :disabled="working"
            @click="restoreBackup(entry)"
          >
            恢复
          </button>
          <button
            v-if="!entry.isDirectory"
            class="btn btn-sm"
            :disabled="working"
            @click="downloadEntry(entry)"
          >
            下载
          </button>
          <button
            v-if="!entry.toParent"
            class="btn btn-sm btn-danger"
            :disabled="working"
            @click="removeEntry(entry)"
          >
            删除
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { useAppStore } from '../stores/app'
import {
  deleteWebdavFile,
  deleteWebdavFileList,
  getWebdavFileBlob,
  getWebdavFileList,
  getWebdavFileText,
  type WebdavFileEntry,
  uploadFilesToWebdav,
  uploadTextToWebdav,
} from '../api/webdav'
import {
  createWebdavBackupPayload,
  parseWebdavBackup,
  restoreWebdavBackup,
  serializeWebdavBackup,
} from '../utils/webdavBackup'
import { formatBytes, formatDateTime } from '../utils/format'

type EntryRow = WebdavFileEntry & { toParent?: boolean }

const appStore = useAppStore()
const router = useRouter()
const fileInputRef = ref<HTMLInputElement | null>(null)
const currentPath = ref('/')
const entries = ref<EntryRow[]>([])
const selectedPaths = ref<string[]>([])
const loading = ref(false)
const working = ref(false)
const errorMessage = ref('')

onMounted(() => {
  if (appStore.isLoggedIn) {
    void loadFiles(currentPath.value)
  }
})

// 深链接直达时 history 里没有上一条记录，回退落到书架页
function goBack() {
  if (window.history.state?.back) {
    router.back()
  } else {
    router.push({ name: 'home' })
  }
}

function isBackupFile(name: string) {
  return name.toLowerCase().endsWith('.json')
}

function toParentPath(path: string) {
  if (path === '/' || !path) return '/'
  const parts = path.split('/').filter(Boolean)
  parts.pop()
  return parts.length ? `/${parts.join('/')}` : '/'
}

function toggleSelection(path: string) {
  if (selectedPaths.value.includes(path)) {
    selectedPaths.value = selectedPaths.value.filter((item) => item !== path)
  } else {
    selectedPaths.value = selectedPaths.value.concat(path)
  }
}

async function loadFiles(path = '/') {
  loading.value = true
  errorMessage.value = ''
  try {
    const list = await getWebdavFileList(path)
    currentPath.value = path
    selectedPaths.value = []
    const rows: EntryRow[] =
      path !== '/'
        ? [
            {
              name: '..',
              size: 0,
              path: toParentPath(path),
              lastModified: 0,
              isDirectory: true,
              toParent: true,
            },
          ]
        : []
    rows.push(
      ...list.sort((a, b) => {
        if (a.isDirectory !== b.isDirectory) return a.isDirectory ? -1 : 1
        return a.name.localeCompare(b.name)
      })
    )
    entries.value = rows
  } catch (error) {
    errorMessage.value = (error as Error).message || '无法读取服务器备份文件列表'
    entries.value = []
  } finally {
    loading.value = false
  }
}

function openEntry(entry: EntryRow) {
  if (entry.isDirectory) {
    void loadFiles(entry.path)
  }
}

function triggerUpload() {
  fileInputRef.value?.click()
}

async function handleUpload(event: Event) {
  const input = event.target as HTMLInputElement
  const files = Array.from(input.files || [])
  if (!files.length) return
  working.value = true
  try {
    await uploadFilesToWebdav(
      files.map((file) => ({ file, name: file.name })),
      currentPath.value
    )
    appStore.showToast('文件已上传到服务器', 'success')
    await loadFiles(currentPath.value)
  } catch (error) {
    appStore.showToast((error as Error).message || '上传失败', 'error')
  } finally {
    working.value = false
    input.value = ''
  }
}

function buildBackupFilename() {
  const now = new Date()
  const pad = (value: number) => String(value).padStart(2, '0')
  return `reader-backup-${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}.json`
}

async function createBackup() {
  working.value = true
  try {
    const payload = await createWebdavBackupPayload()
    await uploadTextToWebdav(serializeWebdavBackup(payload), buildBackupFilename(), '/backups')
    appStore.showToast('备份已保存到 /backups', 'success')
    await loadFiles('/backups')
  } catch (error) {
    appStore.showToast((error as Error).message || '备份失败', 'error')
  } finally {
    working.value = false
  }
}

async function downloadEntry(entry: EntryRow) {
  working.value = true
  try {
    const blob = await getWebdavFileBlob(entry.path)
    const url = URL.createObjectURL(blob)
    const link = document.createElement('a')
    link.href = url
    link.download = entry.name
    document.body.appendChild(link)
    link.click()
    link.remove()
    URL.revokeObjectURL(url)
  } catch (error) {
    appStore.showToast((error as Error).message || '下载失败', 'error')
  } finally {
    working.value = false
  }
}

async function removeEntry(entry: EntryRow) {
  if (!confirm(`确定删除 ${entry.name} 吗？`)) return
  working.value = true
  try {
    await deleteWebdavFile(entry.path)
    appStore.showToast('文件已删除', 'success')
    await loadFiles(currentPath.value)
  } catch (error) {
    appStore.showToast((error as Error).message || '删除失败', 'error')
  } finally {
    working.value = false
  }
}

async function removeSelected() {
  if (!selectedPaths.value.length) return
  if (!confirm(`确定删除选中的 ${selectedPaths.value.length} 个项目吗？`)) return
  working.value = true
  try {
    await deleteWebdavFileList(selectedPaths.value)
    appStore.showToast('选中文件已删除', 'success')
    await loadFiles(currentPath.value)
  } catch (error) {
    appStore.showToast((error as Error).message || '批量删除失败', 'error')
  } finally {
    working.value = false
  }
}

async function restoreBackup(entry: EntryRow) {
  if (!confirm(`确定从 ${entry.name} 恢复数据吗？这会覆盖当前书架、书源、书签和相关本地配置。`)) {
    return
  }

  working.value = true
  try {
    const raw = await getWebdavFileText(entry.path)
    const payload = parseWebdavBackup(raw)
    await restoreWebdavBackup(payload)
    appStore.showToast('恢复完成，正在刷新页面', 'success')
    window.setTimeout(() => {
      window.location.reload()
    }, 800)
  } catch (error) {
    appStore.showToast((error as Error).message || '恢复失败', 'error')
    working.value = false
  }
}
</script>

<style scoped>
.webdav-view {
  height: 100%;
  min-height: 0;
  width: 100%;
  max-width: 1100px;
  margin: 0 auto;
  padding: 0 var(--space-6);
  display: flex;
  flex-direction: column;
}

.page-header {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-5) 0;
  border-bottom: 1px solid var(--color-divider);
  flex-shrink: 0;
}

.title-block {
  min-width: 0;
}

.page-header h2 {
  font-size: var(--text-xl);
  font-weight: 700;
}

.subtitle {
  margin-top: var(--space-1);
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
}

.icon-btn,
.file-main {
  border: none;
  background: none;
  font: inherit;
}

.icon-btn {
  width: 38px;
  height: 38px;
  border-radius: var(--radius-md);
  color: var(--color-text-secondary);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.icon-btn:hover {
  background: var(--color-bg-hover);
}

.icon-btn svg {
  width: 18px;
  height: 18px;
}

.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  padding: var(--space-4) 0;
  border-bottom: 1px solid var(--color-divider);
  flex-shrink: 0;
}

.toolbar-left {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}

.path-bar,
.notice {
  margin: var(--space-4) 0 0;
}

.path-bar {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
}

.path-bar code {
  padding: var(--space-1) var(--space-2);
  border-radius: var(--radius-sm);
  background: var(--color-bg-sunken);
  color: var(--color-text);
}

.notice {
  display: grid;
  gap: 4px;
  padding: var(--space-3) var(--space-4);
  border-radius: var(--radius-md);
  font-size: var(--text-sm);
}

.notice.warning {
  background: rgba(201, 127, 58, 0.12);
  border: 1px solid rgba(201, 127, 58, 0.18);
}

.notice.error {
  background: rgba(245, 34, 45, 0.08);
  border: 1px solid rgba(245, 34, 45, 0.14);
}

.file-list {
  flex: 1;
  overflow: auto;
  padding: var(--space-4) 0 calc(var(--space-6) + var(--safe-area-bottom));
}

.file-row {
  display: grid;
  grid-template-columns: 28px minmax(0, 1fr) 90px 180px auto;
  align-items: center;
  gap: var(--space-3);
  min-height: 56px;
  padding: 0 var(--space-3);
  border-bottom: 1px solid var(--color-divider);
}

.file-row:last-child {
  border-bottom: none;
}

.file-check {
  display: flex;
  align-items: center;
  justify-content: center;
}

.file-check.placeholder {
  width: 20px;
}

.file-main {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  min-width: 0;
  padding: 0;
  text-align: left;
  color: var(--color-text);
}

.file-main.directory .file-name {
  color: var(--color-primary);
}

.file-icon {
  font-size: var(--text-lg);
}

.file-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.file-meta {
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.file-actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-2);
}

.empty-state {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 220px;
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
}

.hidden-input {
  display: none;
}

@media (max-width: 767px) {
  .webdav-view {
    padding: 0 var(--space-4);
  }

  /* 四个按钮同一行排得开；极窄屏换行时删除按钮也贴右 */
  .toolbar {
    flex-wrap: wrap;
  }

  .toolbar > .btn-danger {
    margin-left: auto;
  }

  .file-row {
    grid-template-columns: 28px minmax(0, 1fr);
    padding: var(--space-3);
  }

  .file-meta {
    display: none;
  }

  .file-actions {
    grid-column: 2;
    justify-content: flex-start;
    flex-wrap: wrap;
  }
}
</style>
