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
    <SourceManager v-model="appStore.showSourceManager" />
    <WebdavManager v-model="appStore.showWebdavManager" />
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
import { computed, onMounted, onUnmounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useAppStore } from './stores/app'
import AppTopBar from './components/AppTopBar.vue'
import SettingsDrawer from './components/SettingsDrawer.vue'
import SourceManager from './components/SourceManager.vue'
import WebdavManager from './components/WebdavManager.vue'
import CacheLibraryModal from './components/CacheLibraryModal.vue'
import AiSettingsModal from './components/AiSettingsModal.vue'

const route = useRoute()
const router = useRouter()
const appStore = useAppStore()

const showHeader = computed(() => route.name !== 'reader')

onMounted(() => {
  appStore.fetchUserInfo()
})

async function handleNeedLogin() {
  await appStore.fetchUserInfo()
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
