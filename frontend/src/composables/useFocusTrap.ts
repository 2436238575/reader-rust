import { nextTick, onBeforeUnmount, watch, type Ref } from 'vue'

// 与浏览器默认聚焦规则对齐的可聚焦元素选择器
const FOCUSABLE_SELECTOR = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

/**
 * 轻量焦点捕获（不引第三方库），配合管理类弹层的 v-model 开关使用：
 * - 打开时记录当前焦点（触发元素），并把焦点移入容器内首个可聚焦元素；
 * - Tab / Shift+Tab 在容器内循环，焦点不会落到弹层外面；
 * - 关闭时把焦点还给触发元素（触发元素已被卸载则放弃，焦点自然落到 body）。
 *
 * 与 useEscClose 一样用 watch 跟踪开关：弹层组件常驻挂载、v-model 控制显隐。
 * keydown 挂在容器自身而非 window：焦点被循环约束在容器内，多层弹层堆叠时
 * 各 trap 只响应自己容器里的事件，互不抢焦点。
 * 弹层初始关闭或挂载时已打开都能正确挂上（immediate + 等一次 DOM 更新）。
 */
export function useFocusTrap(active: Ref<boolean>, container: Ref<HTMLElement | undefined>) {
  let trigger: Element | null = null
  let attached: HTMLElement | null = null

  function focusableItems(): HTMLElement[] {
    const root = container.value
    if (!root) return []
    return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR))
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key !== 'Tab' || !active.value) return
    const root = container.value
    if (!root) return
    const items = focusableItems()
    if (items.length === 0) {
      // 空容器吞掉 Tab，避免焦点逃出弹层
      event.preventDefault()
      return
    }
    const first = items[0]
    const last = items[items.length - 1]
    const current = document.activeElement
    if (event.shiftKey) {
      if (current === first || !root.contains(current)) {
        event.preventDefault()
        last.focus()
      }
    } else if (current === last || !root.contains(current)) {
      event.preventDefault()
      first.focus()
    }
  }

  function detach() {
    attached?.removeEventListener('keydown', onKeydown)
    attached = null
  }

  function restoreTrigger() {
    if (trigger instanceof HTMLElement && document.contains(trigger)) {
      trigger.focus()
    }
    trigger = null
  }

  watch(
    active,
    async (open) => {
      if (open) {
        trigger = document.activeElement
        // 容器普遍是 v-if 渲染、模板引用在挂载后才绑定，等一次 DOM 更新再取引用
        await nextTick()
        const root = container.value
        if (!root || !active.value) return
        detach()
        root.addEventListener('keydown', onKeydown)
        attached = root
        focusableItems()[0]?.focus()
      } else {
        detach()
        restoreTrigger()
      }
    },
    // immediate 让「挂载时已经打开」的弹层也能正确挂上（初始关闭时走 else 空转）
    { immediate: true }
  )

  onBeforeUnmount(() => {
    detach()
    if (active.value) restoreTrigger()
  })
}
