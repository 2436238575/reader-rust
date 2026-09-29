<template>
  <Teleport to="body">
    <Transition name="mobile-menu">
      <div v-if="open" class="mobile-menu" role="dialog" aria-modal="true" aria-label="主导航">
        <header class="menu-header">
          <div class="menu-brand">
            <svg
              class="brand-icon"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
            >
              <path d="M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z" />
              <path d="M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z" />
            </svg>
            <span class="brand-text">阅读</span>
          </div>
          <button class="menu-close" type="button" aria-label="关闭菜单" @click="close">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </header>

        <nav class="menu-body">
          <router-link
            v-for="item in items"
            :key="item.key"
            :to="item.path"
            class="menu-row"
            :class="{ active: activeKey === item.key }"
            @click="close"
          >
            <svg
              class="row-icon"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
            >
              <path v-for="(path, index) in item.paths" :key="index" :d="path" />
            </svg>
            <span class="row-label">{{ item.label }}</span>
          </router-link>

          <button class="menu-row" type="button" @click="openSettings">
            <svg
              class="row-icon"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
            >
              <circle cx="12" cy="12" r="3" />
              <path
                d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"
              />
            </svg>
            <span class="row-label">设置</span>
          </button>

          <div class="menu-row appearance">
            <span class="row-label">外观</span>
            <ThemeSwitch />
          </div>
        </nav>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, watch } from 'vue'
import { useRoute } from 'vue-router'
import { useAppStore } from '../stores/app'
import ThemeSwitch from './ThemeSwitch.vue'

type NavKey = 'home' | 'explore' | 'recent'

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ (e: 'close'): void }>()

const route = useRoute()
const appStore = useAppStore()

// 与底部胶囊、顶栏 Tab 用同一份分区定义与图标
const items: Array<{ key: NavKey; label: string; path: string; paths: string[] }> = [
  {
    key: 'home',
    label: '书架',
    path: '/',
    paths: [
      'M4 5.5A2.5 2.5 0 0 1 6.5 3H20',
      'M4 5.5V19a2 2 0 0 0 2 2h14',
      'M8 7h8',
      'M8 11h8',
      'M8 15h5',
    ],
  },
  {
    key: 'explore',
    label: '发现',
    path: '/explore',
    paths: ['M12 3a9 9 0 1 0 9 9', 'm16.24 7.76-2.12 6.36-6.36 2.12 2.12-6.36 6.36-2.12z'],
  },
  {
    key: 'recent',
    label: '最近',
    path: '/recent',
    paths: ['M12 7v5l3 2', 'M12 3a9 9 0 1 0 9 9'],
  },
]

const activeKey = computed<NavKey>(() => {
  if (route.path.startsWith('/explore')) return 'explore'
  if (route.path.startsWith('/recent')) return 'recent'
  return 'home'
})

function close() {
  emit('close')
}

function openSettings() {
  close()
  appStore.showSettingsDrawer = true
}

function handleKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') close()
}

// 桌面端宽度下不需要整屏菜单（顶栏已有 Tab）
const desktopQuery = typeof window !== 'undefined' ? window.matchMedia('(min-width: 768px)') : null
function handleDesktopChange() {
  if (desktopQuery?.matches) close()
}

watch(
  () => props.open,
  (open) => {
    if (typeof document === 'undefined') return
    // 打开时锁滚动，避免背景跟着滑
    document.body.style.overflow = open ? 'hidden' : ''
    if (open) {
      window.addEventListener('keydown', handleKeydown)
    } else {
      window.removeEventListener('keydown', handleKeydown)
    }
  }
)

// 路由变了就收起（点行导航、浏览器前进后退都算）
watch(
  () => route.fullPath,
  () => {
    if (props.open) close()
  }
)

onMounted(() => desktopQuery?.addEventListener('change', handleDesktopChange))
onBeforeUnmount(() => {
  desktopQuery?.removeEventListener('change', handleDesktopChange)
  window.removeEventListener('keydown', handleKeydown)
  document.body.style.overflow = ''
})
</script>

<style scoped>
.mobile-menu {
  position: fixed;
  inset: 0;
  z-index: var(--z-modal);
  display: flex;
  flex-direction: column;
  background: var(--color-bg-elevated);
  color: var(--color-text);
  /* 顶部刘海/状态栏 */
  padding-top: var(--safe-area-top);
  overflow-y: auto;
  overscroll-behavior: contain;
}

.menu-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--space-4) var(--space-5);
}

.menu-brand {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-weight: 700;
}

.brand-icon {
  width: 26px;
  height: 26px;
  color: var(--color-primary);
}

.brand-text {
  font-size: var(--text-lg);
}

.menu-close {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 40px;
  height: 40px;
  border-radius: var(--radius-full);
  color: var(--color-text-secondary);
}

.menu-close svg {
  width: 22px;
  height: 22px;
}

.menu-close:hover {
  background: var(--color-bg-hover);
}

.menu-body {
  display: flex;
  flex-direction: column;
  padding: 0 var(--space-5) var(--space-8);
}

/* 一行一个分区：整行可点，行间用细线分隔（对齐 VitePress 的移动端菜单） */
.menu-row {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  width: 100%;
  padding: var(--space-4) 0;
  border-bottom: 1px solid var(--color-border-light);
  color: var(--color-text);
  font-size: var(--text-base);
  font-weight: 600;
  text-align: left;
}

.menu-row:last-child {
  border-bottom: none;
}

.menu-row.active {
  color: var(--color-primary);
}

.row-icon {
  width: 20px;
  height: 20px;
  flex-shrink: 0;
  color: var(--color-text-tertiary);
}

.menu-row.active .row-icon {
  color: var(--color-primary);
}

.row-label {
  flex: 1;
}

.appearance {
  cursor: default;
}

/* 顶栏汉堡只在移动端出现，菜单本身也不该在桌面端被打开 */
@media (min-width: 768px) {
  .mobile-menu {
    display: none;
  }
}

.mobile-menu-enter-active,
.mobile-menu-leave-active {
  transition:
    opacity 180ms var(--ease-out),
    transform 220ms var(--ease-out);
}

.mobile-menu-enter-from,
.mobile-menu-leave-to {
  opacity: 0;
  transform: translateY(-8px);
}
</style>
