import { nextTick, onActivated, onMounted, onUnmounted } from 'vue'

/**
 * keep-alive 页面 deactivated 时子树脱离文档，Chromium 会丢掉内部滚动容器的
 * 滚动位置（重新插入时归零）。onDeactivated 触发时元素已脱离、读到的是 0，
 * 所以用被动 scroll 监听持续记录，activated 后回填。
 * 仅用于被 keep-alive 缓存的页面里的滚动容器。
 */
export function usePreserveScroll(getContainer: () => HTMLElement | null | undefined) {
  let savedScrollTop = 0
  let savedScrollLeft = 0
  let listeningTo: HTMLElement | null = null

  const onScroll = () => {
    const el = listeningTo
    if (!el) return
    savedScrollTop = el.scrollTop
    savedScrollLeft = el.scrollLeft
  }

  function bind() {
    const el = getContainer() ?? null
    if (listeningTo === el) return
    listeningTo?.removeEventListener('scroll', onScroll)
    listeningTo = el
    el?.addEventListener('scroll', onScroll, { passive: true })
  }

  onMounted(() => {
    void nextTick(bind)
  })

  onActivated(() => {
    void nextTick(() => {
      const el = getContainer()
      if (el && (savedScrollTop || savedScrollLeft)) {
        el.scrollTop = savedScrollTop
        el.scrollLeft = savedScrollLeft
      }
      bind()
    })
  })

  onUnmounted(() => {
    listeningTo?.removeEventListener('scroll', onScroll)
    listeningTo = null
  })
}
