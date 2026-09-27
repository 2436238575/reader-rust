<template>
  <div id="app">
    <AppTopBar v-if="showHeader" />
    <main class="app-main" :class="{ 'without-header': !showHeader }">
      <router-view />
    </main>
    <SettingsDrawer v-model="appStore.showSettingsDrawer" />
    <SourceManager v-model="appStore.showSourceManager" />
    <WebdavManager v-model="appStore.showWebdavManager" />
    <CacheLibraryModal v-model="appStore.showCacheLibrary" />

    <!-- Toast notifications -->
    <div class="toast-container">
      <TransitionGroup name="slide-up">
        <div
          v-for="toast in appStore.toasts"
          :key="toast.id"
          class="toast"
          :class="toast.type"
        >
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
