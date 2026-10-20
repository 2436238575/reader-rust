import { computed, watch } from 'vue'
import type { ComputedRef } from 'vue'

export interface OverlayPanel {
  isOpen: ComputedRef<boolean> | { value: boolean }
  close: () => void
}

/**
 * 覆盖层历史栈：面板打开时压入一条同 URL 的历史标记，移动端返回键弹掉标记时
 * 只关最上层面板而不是退出阅读页；UI 途径（按钮/ESC/遮罩）关闭时主动
 * history.back() 把标记消费掉，保持返回语义一致。
 * 瞬时元素（点按控制条、选字菜单）不参与：它们自动消失，压栈反而会吃掉返回键。
 *
 * panels 按关闭优先级从高到低排列（ESC/返回键先关排头的）。
 */
export function useOverlayHistory(panels: OverlayPanel[]) {
  const anyOverlayPanelOpen = computed(() => panels.some((panel) => panel.isOpen.value))

  let panelHistoryActive = false
  let consumingPanelHistory = false

  // 关闭顺序即 panels 顺序（与原 ESC 关闭链同序）
  function closeTopmostOverlayPanel() {
    const topmost = panels.find((panel) => panel.isOpen.value)
    topmost?.close()
  }

  watch(anyOverlayPanelOpen, (open) => {
    if (open && !panelHistoryActive) {
      history.pushState({ ...(history.state ?? {}), __readerOverlay: true }, '')
      panelHistoryActive = true
    } else if (!open && panelHistoryActive && !consumingPanelHistory) {
      consumingPanelHistory = true
      history.back()
    }
  })

  function handleOverlayPopState() {
    if (consumingPanelHistory) {
      consumingPanelHistory = false
      panelHistoryActive = false
      return
    }
    if (!panelHistoryActive) return
    panelHistoryActive = false
    closeTopmostOverlayPanel()
  }

  return { anyOverlayPanelOpen, closeTopmostOverlayPanel, handleOverlayPopState }
}
