<template>
  <Teleport to="body">
    <Transition name="fade">
      <div v-if="modelValue" class="drawer-overlay" @click="close"></div>
    </Transition>
    <Transition name="slide-right">
      <aside v-if="modelValue" class="settings-drawer">
        <div class="drawer-header">
          <h2>设置</h2>
          <button class="close-btn" @click="close">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </div>

        <div class="drawer-body">
          <section class="drawer-section">
            <h3 class="section-title">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="18" height="18">
                <path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2" />
                <circle cx="12" cy="7" r="4" />
              </svg>
              用户
            </h3>
            <div v-if="appStore.isLoggedIn" class="user-info-card">
              <div class="user-avatar-lg">
                {{ appStore.userInfo?.username?.charAt(0)?.toUpperCase() || 'U' }}
              </div>
              <div class="user-panel">
                <div class="user-card-header">
                  <div class="user-detail">
                    <span class="user-name">{{ appStore.userInfo?.username }}</span>
                    <span class="user-role">{{ appStore.userInfo?.isAdmin ? '管理员' : '普通用户' }}</span>
                  </div>
                  <button class="btn btn-danger" @click="handleLogout">注销</button>
                </div>
                <button class="inline-link" @click="togglePasswordPanel">
                  {{ showPasswordPanel ? '收起修改密码' : '修改密码' }}
                </button>
                <div v-if="showPasswordPanel" class="password-panel embedded">
                  <label class="password-field">
                    <span>当前密码</span>
                    <input v-model="passwordForm.oldPassword" type="password" autocomplete="current-password" />
                  </label>
                  <label class="password-field">
                    <span>新密码</span>
                    <input v-model="passwordForm.newPassword" type="password" autocomplete="new-password" />
                  </label>
                  <label class="password-field">
                    <span>确认新密码</span>
                    <input v-model="passwordForm.confirmPassword" type="password" autocomplete="new-password" />
                  </label>
                  <div class="password-actions">
                    <button class="btn btn-primary" :disabled="changingPassword" @click="handleChangePassword">
                      {{ changingPassword ? '提交中...' : '保存新密码' }}
                    </button>
                  </div>
                </div>
              </div>
            </div>
            <button v-else class="btn btn-primary btn-block" @click="handleLogin">
              登录 / 注册
            </button>
          </section>

          <section class="drawer-section">
            <h3 class="section-title">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="18" height="18">
                <path d="M4 19.5v-15A2.5 2.5 0 0 1 6.5 2H20v20H6.5a2.5 2.5 0 0 1 0-5H20" />
              </svg>
              书源管理
            </h3>
            <div class="btn-group">
              <button class="btn btn-soft" @click="openSourceManager">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16">
                  <path d="M12 20h9M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z" />
                </svg>
                书源管理
              </button>
            </div>
          </section>

          <section class="drawer-section">
            <h3 class="section-title">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="18" height="18">
                <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" />
                <circle cx="9" cy="7" r="4" />
                <path d="M19 8v6" />
                <path d="M22 11h-6" />
              </svg>
              用户管理
            </h3>
            <div class="status-card">
              <span>{{ userManagerTitle }}</span>
              <small>{{ userManagerMessage }}</small>
            </div>
            <div class="btn-group">
              <button class="btn btn-soft" :disabled="!canManageUsers" @click="openUserManager">
                用户管理
              </button>
            </div>
          </section>

          <section class="drawer-section">
            <h3 class="section-title">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="18" height="18">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                <path d="M7 10l5 5 5-5" />
                <path d="M12 15V3" />
              </svg>
              服务器备份
            </h3>
            <div class="status-card">
              <span>{{ webdavStatusTitle }}</span>
              <small>{{ webdavStatusMessage }}</small>
            </div>
            <div class="btn-group">
              <button class="btn btn-soft" :disabled="!canOpenWebdav" @click="openWebdavManager">
                备份与恢复
              </button>
            </div>
          </section>

          <section class="drawer-section">
            <h3 class="section-title">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="18" height="18">
                <path d="M12 16V4" />
                <path d="m7 9 5-5 5 5" />
                <path d="M20 16.5a2.5 2.5 0 0 1-2.5 2.5h-11A2.5 2.5 0 0 1 4 16.5" />
              </svg>
              应用
            </h3>
            <div class="status-card">
              <span>{{ appStore.isOnline ? '在线' : '离线' }}</span>
              <small>{{ appStore.pwaReady ? '已启用离线外壳缓存' : '离线外壳未启用' }}</small>
            </div>
            <div class="status-card">
              <span>{{ appVersion }}</span>
              <small>当前应用版本</small>
            </div>
            <template v-if="appStore.canCheckVersionUpdate">
              <div
                class="status-card"
                :class="{ accent: appStore.hasVersionUpdateReminder, muted: appStore.versionUpdateLoading }"
              >
                <span>{{ versionUpdateTitle }}</span>
                <small>{{ versionUpdateMessage }}</small>
              </div>
              <div class="btn-group version-actions">
                <button class="btn btn-soft" :disabled="!appStore.versionUpdate?.releaseUrl" @click="handleOpenRelease">
                  查看 Release
                </button>
                <button
                  class="btn btn-soft"
                  :disabled="!appStore.hasVersionUpdateReminder || appStore.versionUpdateLoading"
                  @click="handleDismissVersionUpdate"
                >
                  本版本不再提醒
                </button>
                <button class="btn btn-soft" :disabled="appStore.versionUpdateLoading" @click="handleCheckVersionUpdate">
                  {{ appStore.versionUpdateLoading ? '检查中...' : '重新检查' }}
                </button>
              </div>
            </template>
            <div v-if="appStore.pwaUpdateAvailable" class="status-card accent">
              <span>发现新版本</span>
              <small>刷新后可使用最新离线资源</small>
            </div>
            <div class="btn-group">
              <button class="btn btn-soft" :disabled="!appStore.deferredInstallPrompt" @click="handleInstallPwa">
                安装到主屏幕
              </button>
              <button class="btn btn-primary" :disabled="!appStore.pwaUpdateAvailable" @click="handleApplyUpdate">
                更新应用
              </button>
            </div>
          </section>

          <section class="drawer-section">
            <h3 class="section-title">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="18" height="18">
                <rect width="7" height="7" x="3" y="3" rx="1" />
                <rect width="7" height="7" x="14" y="3" rx="1" />
                <rect width="7" height="7" x="3" y="14" rx="1" />
                <rect width="7" height="7" x="14" y="14" rx="1" />
              </svg>
              书架设置
            </h3>
            <div class="btn-group">
              <button class="btn btn-soft" @click="refreshCache">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="16" height="16">
                  <path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
                  <path d="M3 3v5h5" />
                  <path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16" />
                  <path d="M16 16h5v5" />
                </svg>
                刷新缓存
              </button>
            </div>
          </section>

          <section class="drawer-section">
            <h3 class="section-title">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="18" height="18">
                <path d="M12 8v4l3 3" />
                <circle cx="12" cy="12" r="9" />
              </svg>
              阅读统计
            </h3>
            <div class="stats-grid">
              <div class="status-card">
                <span>{{ appStore.readingStatsSummary.totalTimeText }}</span>
                <small>累计阅读时长</small>
              </div>
              <div class="status-card">
                <span>{{ appStore.readingStatsSummary.openedBooks }}</span>
                <small>打开过的书籍</small>
              </div>
              <div class="status-card">
                <span>{{ appStore.readingStatsSummary.readChapters }}</span>
                <small>阅读章节数</small>
              </div>
              <div class="status-card">
                <span>{{ appStore.readingStatsSummary.completedBooks }}</span>
                <small>读完书籍数</small>
              </div>
            </div>
          </section>

          <section class="drawer-section">
            <h3 class="section-title">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="18" height="18">
                <circle cx="12" cy="12" r="4" />
                <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41" />
              </svg>
              外观
            </h3>
            <div class="theme-toggle">
              <button
                class="theme-option"
                :class="{ active: appStore.theme === 'light' }"
                @click="setTheme('light')"
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="20" height="20">
                  <circle cx="12" cy="12" r="4" />
                  <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41" />
                </svg>
                亮色
              </button>
              <button
                class="theme-option"
                :class="{ active: appStore.theme === 'dark' }"
                @click="setTheme('dark')"
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="20" height="20">
                  <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
                </svg>
                暗色
              </button>
            </div>
          </section>
        </div>
      </aside>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useAppStore } from '../stores/app'
import { useBookshelfStore } from '../stores/bookshelf'
import { changePassword, logout as apiLogout } from '../api/user'

const props = defineProps<{
  modelValue: boolean
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
}>()

const appStore = useAppStore()
const shelfStore = useBookshelfStore()
const router = useRouter()
const appVersion = __APP_VERSION__
const showPasswordPanel = ref(false)
const changingPassword = ref(false)
const passwordForm = reactive({
  oldPassword: '',
  newPassword: '',
  confirmPassword: '',
})

const canManageUsers = computed(() => appStore.adminAuthorized)
const userManagerTitle = computed(() => {
  if (!appStore.isLoggedIn) return '登录后可查看状态'
  return appStore.userInfo?.isAdmin ? '当前账号拥有管理员权限' : '当前账号不是管理员'
})
const userManagerMessage = computed(() => {
  if (!appStore.isLoggedIn) return '请先登录管理员账号后管理其他用户。'
  return appStore.userInfo?.isAdmin
    ? '支持新增用户、重置密码、删除用户和调整权限。'
    : '请使用管理员账号登录后再进行用户管理。'
})
const canOpenWebdav = computed(() => appStore.isLoggedIn && !!appStore.userInfo?.enableWebdav)
const webdavStatusTitle = computed(() => {
  if (!appStore.isLoggedIn) return '登录后可用'
  return appStore.userInfo?.enableWebdav ? '当前账号已开启服务器备份' : '当前账号未开启服务器备份'
})
const webdavStatusMessage = computed(() => {
  if (!appStore.isLoggedIn) return '登录并具备备份权限后，可管理服务器中的备份文件。'
  return appStore.userInfo?.enableWebdav
    ? '支持将数据备份到服务器、下载备份文件、上传备份文件并执行恢复。'
    : '请在用户管理中为当前账号开启服务器备份权限。'
})
const versionUpdateTitle = computed(() => {
  const info = appStore.versionUpdate
  if (appStore.versionUpdateLoading && !info) return '正在检查服务端版本'
  if (!info) return '服务端版本检查'
  if (info.error && !info.latestVersion) return '版本检查失败'
  if (info.updateAvailable) return `发现服务端新版本 ${info.latestVersion}`
  return '服务端已是最新版本'
})
const versionUpdateMessage = computed(() => {
  const info = appStore.versionUpdate
  if (appStore.versionUpdateLoading && !info) return '正在从 GitHub Release 获取最新版本。'
  if (!info) return '管理员可检查 GitHub Release，发现新版后会在设置入口提示。'
  if (info.error && !info.latestVersion) return info.error
  if (info.updateAvailable && info.shouldRemind) {
    return `当前 ${info.currentVersion}，最新 ${info.latestVersion}。`
  }
  if (info.updateAvailable) {
    return `当前 ${info.currentVersion}，最新 ${info.latestVersion}，本版本已设置不再提醒。`
  }
  if (info.error) return `当前 ${info.currentVersion}，上次检查失败：${info.error}`
  return `当前 ${info.currentVersion}。`
})

function close() {
  emit('update:modelValue', false)
}

function handleLogin() {
  close()
  router.push({ name: 'login' })
}

async function handleLogout() {
  await apiLogout()
  appStore.clearUser()
  await appStore.fetchUserInfo()
  close()
  shelfStore.fetchBooks()
}

function resetPasswordForm() {
  passwordForm.oldPassword = ''
  passwordForm.newPassword = ''
  passwordForm.confirmPassword = ''
}

function togglePasswordPanel() {
  showPasswordPanel.value = !showPasswordPanel.value
  if (!showPasswordPanel.value) {
    resetPasswordForm()
  }
}

async function handleChangePassword() {
  if (!passwordForm.oldPassword || !passwordForm.newPassword || !passwordForm.confirmPassword) {
    appStore.showToast('请填写完整的密码信息', 'warning')
    return
  }
  if (passwordForm.newPassword !== passwordForm.confirmPassword) {
    appStore.showToast('两次输入的新密码不一致', 'warning')
    return
  }
  changingPassword.value = true
  try {
    await changePassword(passwordForm.oldPassword, passwordForm.newPassword)
    appStore.showToast('密码修改成功', 'success')
    showPasswordPanel.value = false
    resetPasswordForm()
  } catch (error) {
    appStore.showToast((error as Error).message || '密码修改失败', 'error')
  } finally {
    changingPassword.value = false
  }
}

function openSourceManager() {
  close()
  appStore.showSourceManager = true
}

function openUserManager() {
  close()
  appStore.showUserManager = true
}

function openWebdavManager() {
  close()
  appStore.showWebdavManager = true
}

function refreshCache() {
  shelfStore.fetchBooks()
  appStore.showToast('书架已刷新', 'success')
  close()
}

function setTheme(t: 'light' | 'dark') {
  appStore.setTheme(t)
}

async function handleInstallPwa() {
  const accepted = await appStore.installPwa()
  if (!accepted) {
    appStore.showToast('当前环境暂不支持安装，或用户已取消', 'warning')
    return
  }
  appStore.showToast('安装请求已提交', 'success')
}

function handleApplyUpdate() {
  const ok = appStore.applyPwaUpdate()
  if (!ok) {
    appStore.showToast('当前没有可应用的新版本', 'warning')
  }
}

function handleOpenRelease() {
  const url = appStore.versionUpdate?.releaseUrl
  if (!url) return
  window.open(url, '_blank', 'noopener,noreferrer')
}

async function handleDismissVersionUpdate() {
  await appStore.dismissVersionUpdateReminder()
}

async function handleCheckVersionUpdate() {
  await appStore.checkVersionUpdate(true)
}
</script>

<style scoped>
.drawer-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.4);
  z-index: var(--z-overlay);
  backdrop-filter: blur(4px);
}

.settings-drawer {
  position: fixed;
  top: 0;
  right: 0;
  bottom: 0;
  width: min(380px, 90vw);
  background: var(--color-bg-elevated);
  z-index: var(--z-modal);
  display: flex;
  flex-direction: column;
  box-shadow: var(--shadow-xl);
}

.drawer-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: calc(var(--space-5) + var(--safe-area-top)) calc(var(--space-6) + var(--safe-area-right)) var(--space-5) var(--space-6);
  border-bottom: 1px solid var(--color-border-light);
  flex-shrink: 0;
}

.drawer-header h2 {
  font-size: var(--text-xl);
  font-weight: 700;
  letter-spacing: -0.01em;
}

.close-btn {
  width: 36px;
  height: 36px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--radius-md);
  color: var(--color-text-secondary);
  transition: all var(--duration-fast);
}

.close-btn:hover {
  background: var(--color-bg-hover);
  color: var(--color-text);
}

.close-btn svg {
  width: 20px;
  height: 20px;
}

.drawer-body {
  flex: 1;
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
  overscroll-behavior: contain;
  padding: var(--space-4) calc(var(--space-6) + var(--safe-area-right)) calc(var(--space-4) + var(--safe-area-bottom)) var(--space-6);
}

@media (max-width: 768px) {
  .settings-drawer {
    width: min(420px, 92vw);
  }
}

.drawer-section {
  padding: var(--space-4) 0;
  border-bottom: 1px solid var(--color-divider);
}

.drawer-section:last-child {
  border-bottom: none;
}

.section-title {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-size: var(--text-sm);
  font-weight: 600;
  color: var(--color-text-secondary);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: var(--space-3);
}

.user-info-card {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
  padding: var(--space-3);
  background: var(--color-bg-sunken);
  border-radius: var(--radius-md);
}

.user-panel {
  flex: 1;
  display: grid;
  gap: var(--space-3);
}

.user-card-header {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
}

.user-avatar-lg {
  width: 40px;
  height: 40px;
  border-radius: var(--radius-full);
  background: linear-gradient(135deg, var(--color-primary), var(--color-primary-light));
  color: white;
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 700;
  font-size: var(--text-lg);
  flex-shrink: 0;
}

.user-detail {
  flex: 1;
  display: flex;
  flex-direction: column;
}

.user-name {
  font-weight: 600;
  font-size: var(--text-sm);
}

.user-role {
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.password-panel {
  display: grid;
  gap: var(--space-3);
  padding: var(--space-3);
  border: 1px solid var(--color-border-light);
  border-radius: var(--radius-md);
  background: var(--color-bg);
}

.password-panel.embedded {
  background: var(--color-bg-elevated);
}

.password-field {
  display: grid;
  gap: var(--space-2);
}

.password-field span {
  font-size: var(--text-sm);
  color: var(--color-text-secondary);
}

.password-field input {
  min-height: 40px;
  padding: 0 var(--space-3);
  border-radius: var(--radius-md);
  border: 1px solid var(--color-border);
  background: var(--color-bg);
  color: inherit;
}

.password-actions {
  display: flex;
  justify-content: flex-start;
}

.btn-group {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}

.version-actions {
  margin-bottom: var(--space-3);
}

.inline-link {
  padding: 0;
  background: transparent;
  border: none;
  color: var(--color-primary);
  font-weight: 500;
  justify-content: flex-start;
}

.inline-link:hover {
  background: transparent;
  border: none;
  color: var(--color-primary-dark);
}

.theme-toggle {
  display: flex;
  gap: var(--space-2);
}

.status-card {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: var(--space-3);
  background: var(--color-bg-sunken);
  border-radius: var(--radius-md);
  margin-bottom: var(--space-3);
}

.status-card span {
  font-size: var(--text-sm);
  font-weight: 600;
}

.status-card small {
  color: var(--color-text-tertiary);
}

.status-card.accent {
  background: rgba(201, 127, 58, 0.12);
  border: 1px solid rgba(201, 127, 58, 0.18);
}

.status-card.muted {
  opacity: 0.72;
}

.stats-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--space-2);
}

.theme-option {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-4);
  border-radius: var(--radius-md);
  border: 2px solid var(--color-border-light);
  background: var(--color-bg);
  font-size: var(--text-sm);
  font-weight: 500;
  transition: all var(--duration-fast);
  color: var(--color-text-secondary);
}

.theme-option.active {
  border-color: var(--color-primary);
  color: var(--color-primary);
  background: var(--color-primary-bg);
}

.theme-option:hover:not(.active) {
  border-color: var(--color-border);
}
</style>
