<template>
  <div id="app">
    <AppTopBar v-if="showHeader" />
    <main class="app-main" :class="{ 'without-header': !showHeader }">
      <!-- 列表页保活：从阅读器/详情返回时保留滚动位置与已加载数据
           （搜索页此前整轮 SSE 重搜、滚动全丢）；阅读器自身有会话恢复，不保活 -->
      <router-view v-slot="{ Component }">
        <keep-alive :include="['HomeView', 'ExploreView', 'RecentView', 'SearchView']">
          <component :is="Component" />
        </keep-alive>
      </router-view>
    </main>
    <SettingsDrawer v-model="appStore.showSettingsDrawer" />
    <CacheLibraryModal v-model="appStore.showCacheLibrary" />
    <AiSettingsModal v-model="appStore.showAiSettings" />

    <!-- Toast notifications -->
    <div class="toast-container">
      <TransitionGroup name="slide-up">
        <div v-for="toast in appStore.toasts" :key="toast.id" class="toast" :class="toast.type">
          {{ toast.message }}
        </div>
      </TransitionGroup>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, defineAsyncComponent, onMounted, onUnmounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useAppStore } from './stores/app'
import { startAuthLifecycle } from './utils/authSession'
import AppTopBar from './components/AppTopBar.vue'
import SettingsDrawer from './components/SettingsDrawer.vue'

// 低频管理弹窗懒加载：不进首包，首次打开时才拉取对应 chunk
const CacheLibraryModal = defineAsyncComponent(() => import('./components/CacheLibraryModal.vue'))
const AiSettingsModal = defineAsyncComponent(() => import('./components/AiSettingsModal.vue'))

const route = useRoute()
const router = useRouter()
const appStore = useAppStore()

const showHeader = computed(() => route.name !== 'reader')

onMounted(() => {
  // 会话凭据在 HttpOnly Cookie 里：这里先做旧令牌迁移与 Cookie 有效性核验
  // （getUserInfo 为空时刷新再取），避免首屏请求集体 401
  void appStore.bootstrapAuth()
  startAuthLifecycle()
})

async function handleNeedLogin() {
  // 401 触发的重查必须绕过新鲜期：令牌可能刚被服务端吊销，
  // 复用几十秒前的成功结果会漏掉「实际已登出」的真相
  await appStore.fetchUserInfo({ force: true })
  if (appStore.isLoggedIn || route.name === 'login') return
  const redirect = route.fullPath && route.fullPath !== '/' ? route.fullPath : undefined
  router.push({ name: 'login', query: redirect ? { redirect } : {} })
}

onMounted(() => {
  window.addEventListener('need-login', handleNeedLogin)
})

onUnmounted(() => {
  window.removeEventListener('need-login', handleNeedLogin)
})
</script>

<style>
html,
body {
  height: var(--app-height, 100dvh);
  overflow: hidden;
}

#app {
  height: var(--app-height, 100dvh);
  overflow: hidden;
}

.app-main {
  height: calc(var(--app-height, 100dvh) - var(--header-height) - var(--safe-area-top));
  min-height: 0;
  overflow: hidden;
}

.app-main.without-header {
  height: var(--app-height, 100dvh);
}
</style>
