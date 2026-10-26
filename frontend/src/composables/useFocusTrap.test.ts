// @vitest-environment jsdom
import { createApp, nextTick, ref } from 'vue'
import { describe, expect, it } from 'vitest'
import { useFocusTrap } from './useFocusTrap'

/// composable 里用了 onBeforeUnmount，需要宿主组件（同 useReaderTtsControls.test.ts 的写法）
function withSetup<T>(composable: () => T) {
  let result!: T
  const app = createApp({
    setup() {
      result = composable()
      return () => null
    },
  })
  app.mount(document.createElement('div'))
  return { result, unmount: () => app.unmount() }
}

function makePanel(withItems = true) {
  const panel = document.createElement('div')
  const first = document.createElement('button')
  const middle = document.createElement('input')
  const last = document.createElement('button')
  if (withItems) panel.append(first, middle, last)
  document.body.appendChild(panel)
  return { panel, first, middle, last }
}

function pressTab(panel: HTMLElement, shiftKey = false) {
  // cancelable: true —— 默认可取消，preventDefault 才会生效（否则 defaultPrevented 恒为 false）
  const event = new KeyboardEvent('keydown', { key: 'Tab', shiftKey, bubbles: true, cancelable: true })
  panel.dispatchEvent(event)
  return event
}

describe('useFocusTrap', () => {
  it('打开时焦点移入首个可聚焦元素，关闭时还给触发元素', async () => {
    const trigger = document.createElement('button')
    document.body.appendChild(trigger)
    trigger.focus()
    const { panel, first } = makePanel()

    const active = ref(false)
    const container = ref<HTMLElement>()
    withSetup(() => useFocusTrap(active, container))
    container.value = panel

    active.value = true
    await nextTick()
    await nextTick()
    expect(document.activeElement).toBe(first)

    active.value = false
    await nextTick()
    expect(document.activeElement).toBe(trigger)

    panel.remove()
    trigger.remove()
  })

  it('Tab 在末元素循环回首元素，Shift+Tab 在首元素循环回末元素', async () => {
    const { panel, first, last } = makePanel()
    const active = ref(true)
    const container = ref<HTMLElement>()
    withSetup(() => useFocusTrap(active, container))
    container.value = panel
    await nextTick()
    await nextTick()

    last.focus()
    const forward = pressTab(panel)
    expect(forward.defaultPrevented).toBe(true)
    expect(document.activeElement).toBe(first)

    first.focus()
    const backward = pressTab(panel, true)
    expect(backward.defaultPrevented).toBe(true)
    expect(document.activeElement).toBe(last)

    panel.remove()
  })

  it('Tab 在中间元素上不拦截，交给浏览器自然顺序', async () => {
    const { panel, middle } = makePanel()
    const active = ref(true)
    const container = ref<HTMLElement>()
    withSetup(() => useFocusTrap(active, container))
    container.value = panel
    await nextTick()
    await nextTick()

    middle.focus()
    const event = pressTab(panel)
    expect(event.defaultPrevented).toBe(false)
    expect(document.activeElement).toBe(middle)

    panel.remove()
  })

  it('焦点意外落在容器外时，Tab 把它拉回容器内', async () => {
    const { panel, first } = makePanel()
    const outside = document.createElement('button')
    document.body.appendChild(outside)

    const active = ref(true)
    const container = ref<HTMLElement>()
    withSetup(() => useFocusTrap(active, container))
    container.value = panel
    await nextTick()
    await nextTick()

    outside.focus()
    const event = pressTab(panel)
    expect(event.defaultPrevented).toBe(true)
    expect(document.activeElement).toBe(first)

    panel.remove()
    outside.remove()
  })

  it('空容器吞掉 Tab，焦点不外逃', async () => {
    const { panel } = makePanel(false)
    const active = ref(true)
    const container = ref<HTMLElement>()
    withSetup(() => useFocusTrap(active, container))
    container.value = panel
    await nextTick()
    await nextTick()

    const event = pressTab(panel)
    expect(event.defaultPrevented).toBe(true)

    panel.remove()
  })

  it('宿主组件卸载时（弹层仍开着）焦点还给触发元素', async () => {
    const trigger = document.createElement('button')
    document.body.appendChild(trigger)
    trigger.focus()
    const { panel } = makePanel()

    const active = ref(false)
    const container = ref<HTMLElement>()
    const { unmount } = withSetup(() => useFocusTrap(active, container))
    container.value = panel

    active.value = true
    await nextTick()
    await nextTick()
    expect(document.activeElement).not.toBe(trigger)

    unmount()
    expect(document.activeElement).toBe(trigger)

    panel.remove()
    trigger.remove()
  })
})
