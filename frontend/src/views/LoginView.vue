<template>
  <div class="login-page">
    <div class="login-card">
      <div class="login-header">
        <div class="login-logo">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="32" height="32">
            <path d="M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z" />
            <path d="M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z" />
          </svg>
        </div>
        <h2>{{ isLogin ? '登录' : '注册' }}</h2>
        <p class="login-desc">{{ isLogin ? '登录以同步你的阅读数据' : '创建新账号开始阅读' }}</p>
      </div>

      <form class="login-form" @submit.prevent="handleSubmit">
        <div class="form-field">
          <label for="username">用户名</label>
          <input
            id="username"
            v-model="form.username"
            type="text"
            placeholder="请输入用户名"
            required
            autocomplete="username"
          />
        </div>
        <div class="form-field">
          <label for="password">密码</label>
          <input
            id="password"
            v-model="form.password"
            type="password"
            placeholder="请输入密码"
            required
            :autocomplete="isLogin ? 'current-password' : 'new-password'"
          />
        </div>
        <div v-if="!isLogin" class="form-field">
          <label for="invite-code">邀请码</label>
          <input
            id="invite-code"
            v-model="form.code"
            type="text"
            placeholder="没有则留空"
            autocomplete="off"
          />
        </div>

        <button type="submit" class="btn btn-primary btn-block submit-btn" :disabled="submitting">
          <span v-if="submitting" class="btn-spinner"></span>
          {{ isLogin ? '登 录' : '注 册' }}
        </button>
      </form>

      <p class="switch-mode">
        {{ isLogin ? '没有账号？' : '已有账号？' }}
        <a href="#" @click.prevent="isLogin = !isLogin">
          {{ isLogin ? '注册' : '登录' }}
        </a>
      </p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { reactive, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { login, register } from '../api/user'
import { useAppStore } from '../stores/app'
import { useBookshelfStore } from '../stores/bookshelf'

const route = useRoute()
const router = useRouter()
const appStore = useAppStore()
const shelfStore = useBookshelfStore()

const isLogin = ref(true)
const submitting = ref(false)
const form = reactive({
  username: '',
  password: '',
  code: '',
})

function redirectTarget(): string {
  const r = route.query.redirect
  return typeof r === 'string' && r.startsWith('/') && !r.startsWith('/login') ? r : '/'
}

// 已登录用户访问登录页时直接送走；fetchUserInfo 可能尚未返回，用 watch 覆盖
watch(
  () => appStore.isLoggedIn,
  (loggedIn) => {
    if (loggedIn) router.replace(redirectTarget())
  },
  { immediate: true },
)

async function handleSubmit() {
  if (!form.username || !form.password) return
  submitting.value = true
  try {
    const user = isLogin.value
      ? await login(form.username, form.password)
      : await register(form.username, form.password, form.code || undefined)
    appStore.setUser(user)
    appStore.showToast(isLogin.value ? '登录成功' : '注册成功', 'success')
    shelfStore.fetchBooks()
    shelfStore.fetchGroups()
    router.replace(redirectTarget())
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '操作失败', 'error')
  } finally {
    submitting.value = false
  }
}
</script>

<style scoped>
.login-page {
  height: 100%;
  overflow: auto;
  display: flex;
  align-items: center;
  justify-content: center;
  padding:
    calc(var(--space-6) + var(--safe-area-top))
    calc(var(--space-6) + var(--safe-area-right))
    calc(var(--space-6) + var(--safe-area-bottom))
    calc(var(--space-6) + var(--safe-area-left));
}

.login-card {
  width: 100%;
  max-width: 400px;
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-border-light);
  border-radius: var(--radius-xl);
  padding: var(--space-8);
  box-shadow: var(--shadow-lg);
}

.login-header {
  text-align: center;
  margin-bottom: var(--space-8);
}

.login-logo {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 56px;
  height: 56px;
  border-radius: var(--radius-lg);
  background: var(--color-primary-bg);
  color: var(--color-primary);
  margin-bottom: var(--space-4);
}

.login-header h2 {
  font-size: var(--text-xl);
  font-weight: 700;
  margin-bottom: var(--space-2);
}

.login-desc {
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
}

.login-form {
  display: flex;
  flex-direction: column;
  gap: var(--space-5);
}

.form-field {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}

.form-field label {
  font-size: var(--text-sm);
  font-weight: 500;
  color: var(--color-text-secondary);
}

.form-field input {
  padding: var(--space-3) var(--space-4);
  border: 1.5px solid var(--color-border);
  border-radius: var(--radius-md);
  background: var(--color-bg);
  outline: none;
  transition: all var(--duration-fast);
  font-size: var(--text-base);
}

.form-field input:focus {
  border-color: var(--color-primary);
  box-shadow: 0 0 0 3px var(--color-primary-bg);
}

.submit-btn {
  min-height: 42px;
  margin-top: var(--space-2);
}

.btn-spinner {
  width: 16px;
  height: 16px;
  border: 2px solid rgba(255, 255, 255, 0.3);
  border-top-color: white;
  border-radius: 50%;
  animation: spin 0.6s linear infinite;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

.switch-mode {
  text-align: center;
  margin-top: var(--space-6);
  font-size: var(--text-sm);
  color: var(--color-text-tertiary);
}

.switch-mode a {
  color: var(--color-primary);
  font-weight: 500;
}

@media (max-width: 767px) {
  .login-card {
    padding: var(--space-6);
  }
}
</style>
