<template>
  <header class="app-topbar">
    <div class="topbar-inner">
      <div class="topbar-left">
        <div class="logo" @click="goHome">
          <svg class="logo-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z" />
            <path d="M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z" />
          </svg>
          <span class="logo-text">阅读</span>
        </div>

        <form v-if="showGlobalSearch && !isLoginPage" class="search-box" :class="{ focused: searchFocused }"
          role="search" @submit.prevent="handleSearch">
          <input v-model="searchValue" type="text" :placeholder="searchPlaceholder" @focus="searchFocused = true"
            @blur="searchFocused = false" />
          <button v-if="searchValue" class="search-clear" type="button" @click="clearSearch">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
          <button v-if="!isRecentPage" class="search-submit" type="submit" title="搜索" aria-label="搜索"
            :disabled="!canSearch">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <circle cx="11" cy="11" r="8" />
              <path d="m21 21-4.3-4.3" />
            </svg>
          </button>
        </form>
      </div>

      <div class="topbar-right">
        <nav v-if="showNavTabs" class="topbar-nav" aria-label="主导航">
          <router-link v-for="item in navItems" :key="item.key" :to="item.path" class="nav-tab"
            :class="{ active: activeNavKey === item.key }">
            {{ item.label }}
          </router-link>
          <span class="nav-divider" aria-hidden="true"></span>
        </nav>
        <ThemeSwitch class="theme-switch-desktop" />

        <button v-if="!isLoginPage" class="topbar-btn settings-btn" @click="openSettings" title="设置">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path
              d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43-.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
            <circle cx="12" cy="12" r="3" />
          </svg>
        </button>

        <!-- 移动端菜单入口；独立的 v-if，不参与上面「设置/用户」的 else-if 链 -->
        <button
          v-if="!isLoginPage"
          class="topbar-btn menu-btn"
          type="button"
          title="菜单"
          aria-label="打开菜单"
          @click="showMobileMenu = true"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M3 6h18M3 12h18M3 18h18" />
          </svg>
        </button>
      </div>
    </div>

    <AppMobileMenu :open="showMobileMenu" @close="showMobileMenu = false" />
  </header>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useAppStore } from '../stores/app'
import { useBookshelfStore } from '../stores/bookshelf'
import { useExploreStore } from '../stores/explore'
import AppMobileMenu from './AppMobileMenu.vue'
import ThemeSwitch from './ThemeSwitch.vue'

const router = useRouter()
const route = useRoute()
const appStore = useAppStore()
const shelfStore = useBookshelfStore()
const exploreStore = useExploreStore()

const searchFocused = ref(false)
const searchValue = ref('')

// 停留在搜索页时输入框跟随 URL 上的 q（直达链接也能显示当前搜索词）
watch(
  () => [route.path, route.query.q] as const,
  ([path, q]) => {
    if (path === '/search' && typeof q === 'string') {
      searchValue.value = q
    }
  },
  { immediate: true }
)

type NavKey = 'home' | 'explore' | 'recent'

const navItems: Array<{ key: NavKey; label: string; path: string }> = [
  { key: 'home', label: '书架', path: '/' },
  { key: 'explore', label: '发现', path: '/explore' },
  { key: 'recent', label: '最近', path: '/recent' },
]

const activeNavKey = computed<NavKey>(() => {
  if (route.path.startsWith('/explore')) return 'explore'
  if (route.path.startsWith('/recent')) return 'recent'
  return 'home'
})
const showNavTabs = computed(() => route.name !== 'login')
const isLoginPage = computed(() => route.name === 'login')

const showMobileMenu = ref(false)
const showGlobalSearch = computed(() => route.name !== 'login')
const isRecentPage = computed(() => route.path === '/recent')
// 最近页没有自己的搜索框：顶栏搜索框在该页充当最近阅读的过滤器
const searchPlaceholder = computed(() => (isRecentPage.value ? '搜索最近阅读' : '搜索书籍...'))
const canSearch = computed(() => searchValue.value.trim().length > 0)

// 最近页：输入即过滤；进出页面时清空，避免上次的关键词误过滤
watch(
  [isRecentPage, searchValue],
  ([recent, value]) => {
    shelfStore.recentFilter = recent ? value : ''
  },
  { flush: 'post' },
)

watch(isRecentPage, (recent) => {
  if (recent) {
    searchValue.value = ''
  }
})

function goHome() {
  router.replace('/')
}

function handleSearch() {
  const value = searchValue.value.trim()
  if (!value) return
  // 最近页的搜索框只做本地过滤，不跳全局搜索
  if (isRecentPage.value) return

  // 发现页发起的搜索限定在当前浏览的书源内
  const query: Record<string, string> = { q: value }
  if (route.path === '/explore' && exploreStore.activeSourceUrl) {
    query.source = exploreStore.activeSourceUrl
  }
  router.push({ path: '/search', query })
}

function clearSearch() {
  searchValue.value = ''
}


function openSettings() {
  appStore.showSettingsDrawer = true
}
</script>

<style scoped>
.app-topbar {
  position: sticky;
  top: 0;
  z-index: var(--z-sticky);
  min-height: calc(var(--header-height) + var(--safe-area-top));
  padding-top: var(--safe-area-top);
  background: var(--color-bg-elevated);
  border-bottom: 1px solid var(--color-border-light);
  backdrop-filter: blur(12px);
  -webkit-backdrop-filter: blur(12px);
  box-sizing: border-box;
}

.topbar-inner {
  max-width: var(--content-max-width);
  margin: 0 auto;
  min-height: var(--header-height);
  display: flex;
  align-items: center;
  gap: var(--space-5);
  padding: 0 var(--space-6);
}

.topbar-left {
  display: flex;
  align-items: center;
  gap: var(--space-4);
  flex: 1 1 auto;
  min-width: 0;
}

.logo {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  cursor: pointer;
  flex-shrink: 0;
}

.logo-icon {
  width: 28px;
  height: 28px;
  color: var(--color-primary);
}

.logo-text {
  font-size: var(--text-xl);
  font-weight: 700;
  letter-spacing: -0.02em;
  background: linear-gradient(135deg, var(--color-primary), var(--color-primary-dark));
  -webkit-background-clip: text;
  -webkit-text-fill-color: transparent;
  background-clip: text;
}

.search-box {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  height: 36px;
  background: var(--color-bg-sunken);
  border: 1px solid var(--color-border-light);
  border-radius: var(--radius-full);
  padding: 0 6px 0 var(--space-4);
  width: 220px;
  flex: 0 0 auto;
  transition: width var(--duration-normal) var(--ease-out),
    border-color var(--duration-normal) var(--ease-out),
    background var(--duration-normal) var(--ease-out),
    box-shadow var(--duration-normal) var(--ease-out);
}

.search-box.focused {
  width: min(420px, 42vw);
  border-color: var(--color-primary);
  background: var(--color-bg-elevated);
  box-shadow: 0 0 0 3px var(--color-primary-bg);
}

.search-icon {
  width: 18px;
  height: 18px;
  color: var(--color-text-tertiary);
  flex-shrink: 0;
}

.search-box input {
  flex: 1;
  border: none;
  background: none;
  outline: none;
  font-size: var(--text-sm);
  color: var(--color-text);
  min-width: 0;
}

.search-box input::placeholder {
  color: var(--color-text-tertiary);
}

.search-clear {
  width: 18px;
  height: 18px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--color-text-tertiary);
  flex-shrink: 0;
  padding: 0;
}

.search-clear svg {
  width: 14px;
  height: 14px;
}

.search-submit {
  width: 26px;
  height: 26px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--radius-full);
  color: var(--color-text-inverse);
  background: var(--color-primary);
  flex-shrink: 0;
  padding: 0;
  transition: transform var(--duration-fast), opacity var(--duration-fast), background var(--duration-fast);
}

.search-submit:hover:not(:disabled) {
  background: var(--color-primary-dark);
}

.search-submit:active:not(:disabled) {
  transform: scale(0.94);
}

.search-submit:disabled {
  color: var(--color-text-tertiary);
  background: transparent;
  opacity: 0.75;
}

.search-submit svg {
  width: 15px;
  height: 15px;
}

.topbar-right {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  flex: 0 0 auto;
  margin-left: auto;
}

.topbar-nav {
  display: flex;
  align-items: center;
  gap: var(--space-1);
  margin-right: var(--space-2);
}

.nav-tab {
  padding: var(--space-2) var(--space-4);
  font-size: var(--text-base);
  font-weight: 500;
  color: var(--color-text-secondary);
  border-radius: var(--radius-md);
  white-space: nowrap;
  transition: color var(--duration-fast) var(--ease-out);
}

.nav-tab:hover {
  color: var(--color-text);
}

.nav-tab.active {
  color: var(--color-primary);
  font-weight: 600;
}

.nav-divider {
  width: 1px;
  height: 20px;
  background: var(--color-border);
  margin-left: var(--space-3);
  flex-shrink: 0;
}

@media (max-width: 767px) {
  .topbar-nav {
    display: none;
  }
}

/* 移动端把入口收进整屏菜单：主题切换在菜单的「外观」开关里，
   顶栏不再单独放一个（桌面端仍保留顶栏主题按钮） */
@media (max-width: 767px) {
  /* 移动端入口收进整屏菜单：主题用「外观」开关，设置用「设置」行 */
  .topbar-btn.theme-btn,
  .topbar-btn.settings-btn,
  .theme-switch-desktop {
    display: none;
  }
}

/* 汉堡菜单只在移动端出现（桌面端用顶栏 Tab 导航）；
   复合选择器是为了不被后面的 .topbar-btn { display: flex } 盖掉 */
.topbar-btn.menu-btn {
  display: none;
}

@media (max-width: 767px) {
  .topbar-btn.menu-btn {
    display: flex;
  }
}

.topbar-btn {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: center;
  min-width: 42px;
  min-height: 42px;
  padding: 10px;
  border-radius: var(--radius-full);
  color: var(--color-text-secondary);
  transition: all var(--duration-fast) var(--ease-out);
}


.topbar-btn:hover {
  background: var(--color-bg-elevated);
  color: var(--color-text);
  box-shadow: 0 6px 16px rgba(0, 0, 0, 0.06);
}

.topbar-btn:active {
  background: var(--color-bg-active);
  transform: scale(0.97);
}

.topbar-btn svg {
  width: 20px;
  height: 20px;
  flex-shrink: 0;
}

.user-avatar {
  width: 30px;
  height: 30px;
  border-radius: var(--radius-full);
  background: linear-gradient(135deg, var(--color-primary), var(--color-primary-light));
  color: white;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: var(--text-sm);
  font-weight: 600;
}

@media (max-width: 640px) {
  .topbar-inner {
    padding: 0 var(--space-3);
    gap: var(--space-2);
  }

  .logo-text {
    display: none;
  }

  .search-box {
    width: auto;
    flex: 1 1 auto;
    gap: 6px;
    padding: 0 6px 0 var(--space-3);
  }

  .search-box.focused {
    width: auto;
  }

  .search-submit {
    width: 28px;
    height: 28px;
  }

  .topbar-left {
    gap: var(--space-3);
  }

  .topbar-btn {
    min-width: 38px;
    min-height: 38px;
    padding: 8px;
  }

  .topbar-btn svg {
    width: 18px;
    height: 18px;
  }

  .user-avatar {
    width: 28px;
    height: 28px;
    font-size: var(--text-xs);
  }
}
</style>
