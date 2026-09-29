import { onUnmounted, watch, type Ref } from 'vue'

// ESC 关闭栈：弹层可堆叠（如分组选择叠在分组管理上），只有最上层响应 ESC
const escStack: symbol[] = []

/**
 * 弹层打开期间按 ESC 关闭自己；只响应最上层。
 * 监听挂在 window 上、组件卸载即移除——弹层组件多是常驻挂载 + v-model 控制显隐，
 * 所以开关状态用 watch 跟踪进出栈，而不是组件生命周期。
 */
export function useEscClose(active: Ref<boolean>, close: () => void) {
  const id = Symbol('esc-layer')

  watch(
    active,
    (open) => {
      const idx = escStack.indexOf(id)
      if (open && idx < 0) escStack.push(id)
      if (!open && idx >= 0) escStack.splice(idx, 1)
    },
    { immediate: true }
  )

  const onKey = (e: KeyboardEvent) => {
    if (e.key !== 'Escape' || !active.value) return
    if (escStack[escStack.length - 1] !== id) return
    close()
  }

  window.addEventListener('keydown', onKey)
  onUnmounted(() => {
    window.removeEventListener('keydown', onKey)
    const idx = escStack.indexOf(id)
    if (idx >= 0) escStack.splice(idx, 1)
  })
}
