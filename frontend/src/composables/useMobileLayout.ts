import { onBeforeUnmount, onMounted, ref } from 'vue'

/**
 * 管理类弹窗的「移动端整屏」断点（全站统一 767px，见 AGENTS.md 前端 UI 约定）。
 * 返回的 `isMobileLayout` 用于切换整屏形态与入场动画（mobile slide-right / desktop scale）。
 */
export function useMobileLayout() {
  const mobileQuery =
    typeof window !== 'undefined' && window.matchMedia
      ? window.matchMedia('(max-width: 767px)')
      : null
  const isMobileLayout = ref(mobileQuery?.matches ?? false)
  const handleLayoutChange = (event: MediaQueryListEvent) => {
    isMobileLayout.value = event.matches
  }

  onMounted(() => {
    isMobileLayout.value = mobileQuery?.matches ?? false
    mobileQuery?.addEventListener('change', handleLayoutChange)
  })
  onBeforeUnmount(() => {
    mobileQuery?.removeEventListener('change', handleLayoutChange)
  })

  return { isMobileLayout }
}
