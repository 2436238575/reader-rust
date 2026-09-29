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
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                width="18"
                height="18"
              >
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
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                width="18"
                height="18"
              >
                <path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2" />
                <circle cx="12" cy="7" r="4" />
              </svg>
              用户
            </h3>
            <div v-if="appStore.isLoggedIn" class="user-info-card">
              <div class="user-card-header">
                <span class="user-name">{{ displayName }}</span>
                <div class="user-actions">
                  <button class="btn btn-soft" @click="togglePasswordPanel">
                    {{ showPasswordPanel ? '收起修改密码' : '修改密码' }}
                  </button>
                  <button class="btn btn-danger" @click="handleLogout">退出登录</button>
                </div>
              </div>
              <form
                v-if="showPasswordPanel"
                class="password-panel embedded"
                @submit.prevent="handleChangePassword"
              >
                <label class="password-field">
                  <span>当前密码</span>
                  <input
                    ref="oldPasswordInputRef"
                    v-model="passwordForm.oldPassword"
                    type="password"
                    autocomplete="current-password"
                  />
                </label>
                <label class="password-field">
                  <span>新密码</span>
                  <input
                    v-model="passwordForm.newPassword"
                    type="password"
                    autocomplete="new-password"
                  />
                </label>
                <label class="password-field">
                  <span>确认新密码</span>
                  <input
                    v-model="passwordForm.confirmPassword"
                    type="password"
                    autocomplete="new-password"
                  />
                </label>
                <div class="password-actions">
                  <button class="btn btn-primary" type="submit" :disabled="changingPassword">
                    {{ changingPassword ? '提交中...' : '保存新密码' }}
                  </button>
                </div>
              </form>
            </div>
            <button v-else class="btn btn-primary btn-block" @click="handleLogin">登录</button>
          </section>

          <section class="drawer-section">
            <h3 class="section-title">
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                width="18"
                height="18"
              >
                <path d="M4 19.5v-15A2.5 2.5 0 0 1 6.5 2H20v20H6.5a2.5 2.5 0 0 1 0-5H20" />
              </svg>
              管理
            </h3>
            <div class="btn-group">
              <button class="btn btn-soft" @click="openSourceManager">
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  width="16"
                  height="16"
                >
                  <path d="M12 20h9M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z" />
                </svg>
                书源管理
              </button>
              <button
                class="btn btn-soft"
                :disabled="!appStore.isLoggedIn"
                @click="openCacheLibrary"
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  width="16"
                  height="16"
                >
                  <ellipse cx="12" cy="5" rx="8" ry="3" />
                  <path d="M4 5v6c0 1.66 3.58 3 8 3s8-1.34 8-3V5" />
                  <path d="M4 11v6c0 1.66 3.58 3 8 3s8-1.34 8-3v-6" />
                </svg>
                缓存管理
              </button>
              <button
                class="btn btn-soft"
                :disabled="!appStore.isLoggedIn"
                @click="openWebdavManager"
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  width="18"
                  height="18"
                >
                  <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                  <path d="M7 10l5 5 5-5" />
                  <path d="M12 15V3" />
                </svg>
                备份与恢复
              </button>
            </div>
          </section>

          <section class="drawer-section">
            <h3 class="section-title">
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                width="18"
                height="18"
              >
                <path d="M12 16V4" />
                <path d="m7 9 5-5 5 5" />
                <path d="M20 16.5a2.5 2.5 0 0 1-2.5 2.5h-11A2.5 2.5 0 0 1 4 16.5" />
              </svg>
              应用
            </h3>
            <div class="stats-grid">
              <div class="status-card">
                <span>{{ appStore.isOnline ? '在线' : '离线' }}</span>
                <small>{{ appStore.pwaReady ? '已启用离线外壳缓存' : '离线外壳未启用' }}</small>
              </div>
              <div class="status-card">
                <span>{{ appVersion }}</span>
                <small>当前应用版本</small>
              </div>
            </div>
            <div v-if="appStore.pwaUpdateAvailable" class="status-card accent">
              <span>发现新版本</span>
              <small>刷新后可使用最新离线资源</small>
            </div>
            <div class="btn-group">
              <button
                class="btn btn-soft"
                :disabled="!appStore.deferredInstallPrompt"
                @click="handleInstallPwa"
              >
                安装到主屏幕
              </button>
              <button
                class="btn btn-primary"
                :disabled="!appStore.pwaUpdateAvailable"
                @click="handleApplyUpdate"
              >
                更新应用
              </button>
            </div>
          </section>
        </div>
      </aside>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, nextTick, reactive, ref, toRef } from 'vue'
import { useEscClose } from '../composables/useEscClose'
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

useEscClose(toRef(props, 'modelValue'), () => emit('update:modelValue', false))

const appStore = useAppStore()
const shelfStore = useBookshelfStore()
const router = useRouter()
const appVersion = __APP_VERSION__
const showPasswordPanel = ref(false)

// 用户名首字母大写展示（账号本身保持原样）
const displayName = computed(() => {
  const name = appStore.userInfo?.username || ''
  return name ? name.charAt(0).toUpperCase() + name.slice(1) : ''
})
const changingPassword = ref(false)
const passwordForm = reactive({
  oldPassword: '',
  newPassword: '',
  confirmPassword: '',
})

function close() {
  emit('update:modelValue', false)
}

function handleLogin() {
  close()
  router.push({ name: 'login' })
}

async function handleLogout() {
  if (loggingOut.value) return
  loggingOut.value = true
  try {
    await apiLogout()
    appStore.clearUser()
    await appStore.fetchUserInfo()
    close()
    shelfStore.fetchBooks()
  } finally {
    loggingOut.value = false
  }
}

function resetPasswordForm() {
  passwordForm.oldPassword = ''
  passwordForm.newPassword = ''
  passwordForm.confirmPassword = ''
}

const oldPasswordInputRef = ref<HTMLInputElement>()
const loggingOut = ref(false)

function togglePasswordPanel() {
  showPasswordPanel.value = !showPasswordPanel.value
  if (!showPasswordPanel.value) {
    resetPasswordForm()
  } else {
    // 展开即聚焦第一个输入框
    void nextTick(() => oldPasswordInputRef.value?.focus())
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
    const updated = await changePassword(passwordForm.oldPassword, passwordForm.newPassword)
    // 后端改密时自增 token_version 并为当前设备换发新令牌，必须保存，
    // 否则当前设备会被静默登出
    if (updated?.accessToken) {
      appStore.setAccessToken(updated.accessToken)
    }
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

function openWebdavManager() {
  close()
  appStore.showWebdavManager = true
}

function openCacheLibrary() {
  close()
  appStore.showCacheLibrary = true
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
</script>

<style scoped>
.drawer-overlay {
  position: fixed;
  inset: 0;
  background: var(--overlay-mask-bg);
  z-index: var(--z-overlay);
  -webkit-backdrop-filter: var(--overlay-mask-blur);
  backdrop-filter: var(--overlay-mask-blur);
}

.settings-drawer {
  position: fixed;
  top: 0;
  right: 0;
  bottom: 0;
  width: var(--sidebar-width);
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
  padding: calc(var(--space-5) + var(--safe-area-top)) calc(var(--space-6) + var(--safe-area-right))
    var(--space-5) var(--space-6);
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
  padding: var(--space-4) calc(var(--space-6) + var(--safe-area-right))
    calc(var(--space-4) + var(--safe-area-bottom)) var(--space-6);
}

/* 移动端与导航菜单一样整屏铺开；遮罩被完全盖住，直接隐藏 */
@media (max-width: 767px) {
  .settings-drawer {
    width: 100%;
  }

  .drawer-overlay {
    display: none;
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
  display: grid;
  gap: var(--space-3);
  padding: var(--space-3);
  background: var(--color-bg-sunken);
  border-radius: var(--radius-md);
}

.user-card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
}

.user-actions {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}

/* 修改密码沿用原内联链接的主题色文字，与红色「退出登录」形成区分 */
.user-actions .btn-soft {
  color: var(--color-primary);
}

.user-actions .btn-soft:hover {
  color: var(--color-primary-dark);
}

.user-name {
  font-weight: 600;
  font-size: var(--text-sm);
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
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

/* 应用区：状态卡网格与下方按钮排拉开一行间距 */
.stats-grid + .btn-group {
  margin-top: var(--space-3);
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

.stats-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--space-2);
}

.stats-grid .status-card {
  margin-bottom: 0;
}
</style>
