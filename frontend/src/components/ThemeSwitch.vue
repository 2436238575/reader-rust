<template>
  <button
    class="theme-switch"
    type="button"
    role="switch"
    :aria-checked="isDark"
    aria-label="切换深浅色"
    title="切换深浅色"
    @click="appStore.toggleTheme()"
  >
    <span class="switch-knob">
      <svg v-if="isDark" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
      </svg>
      <svg v-else viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <circle cx="12" cy="12" r="4" />
        <path
          d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"
        />
      </svg>
    </span>
  </button>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useAppStore } from '../stores/app'

const appStore = useAppStore()
const isDark = computed(() => appStore.theme === 'dark')
</script>

<style scoped>
/* 深浅色开关：一枚胶囊 + 圆钮，桌面端顶栏与移动端整屏菜单共用 */
.theme-switch {
  position: relative;
  display: inline-flex;
  align-items: center;
  width: 44px;
  height: 24px;
  padding: 2px;
  border-radius: 999px;
  background: var(--color-bg-sunken);
  border: 1px solid var(--color-border);
  transition: background var(--duration-fast) var(--ease-out);
}

.theme-switch[aria-checked='true'] {
  background: var(--color-primary-bg);
  border-color: var(--color-primary-light);
}

.switch-knob {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: var(--color-bg-elevated);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.18);
  transform: translateX(0);
  transition: transform var(--duration-fast) var(--ease-out);
}

.theme-switch[aria-checked='true'] .switch-knob {
  transform: translateX(20px);
}

.switch-knob svg {
  width: 12px;
  height: 12px;
  color: var(--color-text-secondary);
}
</style>
