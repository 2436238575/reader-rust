import { onBeforeUnmount, onMounted, ref } from 'vue'
import { MOBILE_LAYOUT_QUERY } from '../utils/breakpoints'

/**
 * 管理类弹窗的「移动端整屏」断点（全站统一 767px，口径见 utils/breakpoints.ts）。
 * 返回的 `isMobileLayout` 用于切换整屏形态与入场动画（mobile slide-right / desktop scale）。
 */
export function useMobileLayout() {
  const mobileQuery =
    typeof window !== 'undefined' && window.matchMedia
      ? window.matchMedia(MOBILE_LAYOUT_QUERY)
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
