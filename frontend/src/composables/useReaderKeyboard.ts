import type { useReaderStore } from '../stores/reader'
import type { useAppStore } from '../stores/app'

type ReaderStore = ReturnType<typeof useReaderStore>
type AppStore = ReturnType<typeof useAppStore>

export interface ReaderKeyboardPanels {
  /// ESC 关闭链（顺序即优先级，与原 handleKeydown 的 if 链一致）：
  /// activePanel → 选字菜单 → 搜索 → 评论 → 图片预览 → TTS 面板 → 书籍详情
  escChain: Array<{ isOpen: () => boolean; close: () => void }>
  /// 覆盖型面板打开时翻页/滚屏键不穿透（TTS 面板是悬浮小条，不拦——
  /// 边听边用键盘翻页是正常用法）
  isBlockingOverlayOpen: () => boolean
  /// 点按控制条（瞬时 UI，不进历史栈）
  isControlsVisible: () => boolean
  hideControls: () => void
}

export interface ReaderKeyboardActions {
  goBack: () => void
  pageForward: () => void
  pageBackward: () => void
  nextChapter: () => void
  prevChapter: () => void
  scrollToTop: () => void
  scrollToBottom: () => void
  /// 竖滚模式按 0.8 屏滚（横翻模式的上下键走 pageForward/pageBackward）
  scrollPageVertical: (direction: 1 | -1) => void
  isHorizontalPageMode: () => boolean
  /// 滚动容器未挂载时所有翻页/滚屏键都不动作（与原版行为一致）
  hasScrollContainer: () => boolean
  /// 滚屏/翻页键视为手动接管：先停自动滚动，再继续本次按键动作
  interruptAutoScroll: () => void
}

/// 阅读器键盘快捷键：ESC 关闭链（穿透顺序固定）+ 翻页/滚屏/章跳。
/// App 级弹窗（设置抽屉/缓存库）有自己的 ESC 处理，不穿透到本链。
export function useReaderKeyboard(
  store: ReaderStore,
  appStore: AppStore,
  panels: ReaderKeyboardPanels,
  actions: ReaderKeyboardActions
) {
  function handleKeydown(e: KeyboardEvent) {
    const activeElement = document.activeElement as HTMLElement | null
    const tagName = activeElement?.tagName?.toLowerCase()
    const isEditing =
      tagName === 'input' ||
      tagName === 'textarea' ||
      tagName === 'select' ||
      !!activeElement?.isContentEditable

    // Escape 优先于输入框守卫：焦点在搜索框里时也要能 ESC 关面板（先交还焦点）
    if (e.key === 'Escape') {
      if (appStore.showSettingsDrawer || appStore.showCacheLibrary) return
      if (isEditing) activeElement?.blur()
      for (const panel of panels.escChain) {
        if (panel.isOpen()) {
          panel.close()
          return
        }
      }
      if (panels.isControlsVisible()) {
        panels.hideControls()
        return
      }
      actions.goBack()
      return
    }
    if (isEditing) return

    // 焦点停在按钮/链接上时按空格会同时触发原生点击与翻页：放行原生行为
    if ((tagName === 'button' || tagName === 'a') && e.key === ' ') return

    // 覆盖型面板打开时，翻页/滚屏键不穿透到背后的正文
    if (panels.isBlockingOverlayOpen()) return

    // 滚屏/翻页键视为手动接管：先停自动滚动，再继续本次按键动作
    if (
      store.isAutoScrolling &&
      [' ', 'Space', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'PageUp', 'PageDown', 'Home', 'End'].includes(
        e.key
      )
    ) {
      actions.interruptAutoScroll()
    }

    if (!actions.hasScrollContainer()) return

    const horizontal = actions.isHorizontalPageMode()
    switch (e.key) {
      case ' ':
      case 'Space':
        e.preventDefault()
        actions.pageForward()
        break
      case 'ArrowDown':
      case 'PageDown':
        e.preventDefault()
        if (horizontal) actions.pageForward()
        else actions.scrollPageVertical(1)
        break
      case 'ArrowUp':
      case 'PageUp':
        e.preventDefault()
        if (horizontal) actions.pageBackward()
        else actions.scrollPageVertical(-1)
        break
      case 'ArrowRight':
        e.preventDefault()
        actions.nextChapter()
        break
      case 'ArrowLeft':
        e.preventDefault()
        actions.prevChapter()
        break
      case 'Home':
        e.preventDefault()
        actions.scrollToTop()
        break
      case 'End':
        e.preventDefault()
        actions.scrollToBottom()
        break
    }
  }

  return { handleKeydown }
}
