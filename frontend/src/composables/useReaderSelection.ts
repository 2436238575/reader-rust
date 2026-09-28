import { ref } from 'vue'
import type { ComputedRef, Ref } from 'vue'
import type { useReaderStore } from '../stores/reader'
import type { useAppStore } from '../stores/app'
import { saveReplaceRule } from '../api/replaceRule'

type ReaderStore = ReturnType<typeof useReaderStore>
type AppStore = ReturnType<typeof useAppStore>

export function useReaderSelection(
  store: ReaderStore,
  appStore: AppStore,
  config: ComputedRef<{ selectAction: 'popup' | 'contextmenu' | 'ignore' }>,
  scrollContainerRef: Ref<HTMLElement | undefined>,
) {
  const isTouchDevice = typeof window !== 'undefined'
    && ('ontouchstart' in window || navigator.maxTouchPoints > 0)
  const isAndroid = typeof navigator !== 'undefined' && /Android/i.test(navigator.userAgent)
  const selectionMenu = ref({
    visible: false,
    text: '',
    top: 0,
    left: 0,
    /** popup=随选区自动弹出的胶囊；context=右键唤出的竖排菜单 */
    variant: 'popup' as 'popup' | 'context',
  })
  const activeSelectionText = ref('')
  const suppressSelectionCloseUntil = ref(0)
  let selectionMenuUpdateTimer: number | null = null
  // 「操作弹窗」的弹出许可：只在鼠标左键/触摸释放时置位（拖动选择过程中的
  // selectionchange 只收起不弹出，桌面端才不会还没松手弹窗就跟着光标跳）。
  // touchSession 让触摸端的后续 selectionchange（长按句柄微调）继续弹；
  // 一次真实的 mouseup 会把它关掉，鼠标用户不受触摸屏能力上报影响。
  let releaseLatch = false
  let touchSession = false

  function hideSelectionMenu() {
    selectionMenu.value.visible = false
  }

  function scheduleSelectionMenuUpdate(delay = 220) {
    if (selectionMenuUpdateTimer) {
      clearTimeout(selectionMenuUpdateTimer)
    }
    selectionMenuUpdateTimer = window.setTimeout(() => {
      updateSelectionMenu()
    }, delay)
  }

  function handleMouseUpSelection(event: MouseEvent) {
    // 右键也会触发 mouseup（button=2），不处理否则会把刚唤出的右键菜单收掉
    if (event.button !== 0) return
    releaseLatch = true
    touchSession = false
    scheduleSelectionMenuUpdate(120)
  }

  /** 新的按下动作让上一次「释放」留下的弹出许可失效。
   *
   * 否则「点一下再按住慢慢拖选」会带着前一次单击 mouseup 的旧闩，
   * 拖选还没松手弹窗就先弹出来了。 */
  function handlePressStartSelection() {
    releaseLatch = false
    touchSession = false
  }

  function handleTouchEndSelection() {
    releaseLatch = true
    touchSession = true
    scheduleSelectionMenuUpdate(260)
  }

  function handleSelectionChange() {
    scheduleSelectionMenuUpdate(isAndroid ? 320 : 220)
  }

  function updateSelectionMenu() {
    const selection = window.getSelection?.()
    const text = selection?.toString().trim() || ''
    const hasSelection = !!selection && selection.rangeCount > 0 && !!text && !selection.isCollapsed

    if (config.value.selectAction === 'contextmenu') {
      // 右键菜单模式：菜单只由 contextmenu 事件唤出；选区消失才收起
      if (!hasSelection) hideSelectionMenu()
      return
    }
    if (config.value.selectAction !== 'popup') {
      hideSelectionMenu()
      return
    }
    if (!hasSelection) {
      hideSelectionMenu()
      return
    }
    if (isTouchDevice && text.length < (isAndroid ? 2 : 4)) {
      hideSelectionMenu()
      return
    }
    // 只有「释放」触发的更新才允许弹出（触摸会话里 selectionchange 也算）
    if (!releaseLatch && !touchSession) {
      hideSelectionMenu()
      return
    }
    if (releaseLatch) releaseLatch = false

    const container = scrollContainerRef.value
    const range = selection.getRangeAt(0)
    const commonAncestor = range.commonAncestorContainer
    const targetNode = commonAncestor.nodeType === Node.TEXT_NODE ? commonAncestor.parentElement : commonAncestor as HTMLElement | null
    if (!container || !targetNode || !container.contains(targetNode)) {
      hideSelectionMenu()
      return
    }

    const rect = range.getBoundingClientRect()
    if (!rect.width && !rect.height) {
      hideSelectionMenu()
      return
    }
    suppressSelectionCloseUntil.value = Date.now() + 250
    activeSelectionText.value = text
    selectionMenu.value = {
      visible: true,
      text: text.length > 48 ? `${text.slice(0, 48)}...` : text,
      variant: 'popup',
      top: isTouchDevice
        ? Math.min(window.innerHeight - 76, Math.max(16 + Math.max(0, parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--safe-area-top')) || 0), rect.bottom + 12))
        : Math.max(16 + Math.max(0, parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--safe-area-top')) || 0), rect.top - 56),
      left: Math.min(window.innerWidth - 240, Math.max(16, rect.left + rect.width / 2 - 110)),
    }
  }

  async function addSelectionBookmark() {
    const selection = window.getSelection?.()
    const text = activeSelectionText.value || selection?.toString().trim() || ''
    if (!text) return
    try {
      const pos = scrollContainerRef.value?.scrollTop || 0
      await store.addBookmark(pos, text)
      selection?.removeAllRanges()
      activeSelectionText.value = ''
      hideSelectionMenu()
      appStore.showToast('已加入书签', 'success')
    } catch {
      appStore.showToast('加入书签失败', 'error')
    }
  }

  async function addSelectionReplaceRule(mode: 'book' | 'source') {
    const selection = window.getSelection?.()
    const text = activeSelectionText.value || selection?.toString().trim() || ''
    if (!text || !store.book) return

    try {
      const scope = mode === 'source'
        ? `source:${store.book.origin}`
        : `book:${store.book.bookUrl}`
      await saveReplaceRule({
        id: 0,
        name: `${mode === 'source' ? '书源替换' : '本书替换'} ${store.replaceRules.length + 1}`,
        pattern: text,
        replacement: '',
        scope,
        isEnabled: true,
        isRegex: false,
        order: store.replaceRules.length + 1,
      })
      await store.fetchReplaceRules()
      selection?.removeAllRanges()
      activeSelectionText.value = ''
      hideSelectionMenu()
      appStore.showToast(mode === 'source' ? '已加入书源替换规则' : '已加入本书替换规则', 'success')
    } catch {
      appStore.showToast('加入替换规则失败', 'error')
    }
  }

  /** 右键菜单模式：在选区上右键时唤出菜单（定位跟随光标），返回是否已唤出。 */
  function showSelectionMenuAt(event: MouseEvent): boolean {
    if (config.value.selectAction !== 'contextmenu') return false
    const selection = window.getSelection?.()
    const text = selection?.toString().trim() || ''
    if (!selection || selection.rangeCount === 0 || !text || selection.isCollapsed) {
      return false
    }
    const container = scrollContainerRef.value
    const range = selection.getRangeAt(0)
    const commonAncestor = range.commonAncestorContainer
    const targetNode = commonAncestor.nodeType === Node.TEXT_NODE ? commonAncestor.parentElement : commonAncestor as HTMLElement | null
    if (!container || !targetNode || !container.contains(targetNode)) {
      return false
    }
    // 桌面右键是显式动作，不套触摸端的最短选区长度门槛
    suppressSelectionCloseUntil.value = Date.now() + 250
    activeSelectionText.value = text
    // 像原生右键菜单：以鼠标位置为左上角，视口内夹紧（菜单约 4 行高）
    const safeTop = 8 + Math.max(0, parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--safe-area-top')) || 0)
    selectionMenu.value = {
      visible: true,
      text: text.length > 48 ? `${text.slice(0, 48)}...` : text,
      variant: 'context',
      top: Math.max(safeTop, Math.min(window.innerHeight - 190, event.clientY)),
      left: Math.max(8, Math.min(window.innerWidth - 230, event.clientX)),
    }
    return true
  }

  function clearSelectionState() {
    activeSelectionText.value = ''
    hideSelectionMenu()
    window.getSelection?.()?.removeAllRanges()
  }

  function disposeSelection() {
    if (selectionMenuUpdateTimer) {
      clearTimeout(selectionMenuUpdateTimer)
      selectionMenuUpdateTimer = null
    }
  }

  return {
    selectionMenu,
    suppressSelectionCloseUntil,
    hideSelectionMenu,
    scheduleSelectionMenuUpdate,
    showSelectionMenuAt,
    handleMouseUpSelection,
    handlePressStartSelection,
    handleTouchEndSelection,
    handleSelectionChange,
    updateSelectionMenu,
    addSelectionBookmark,
    addSelectionReplaceRule,
    clearSelectionState,
    disposeSelection,
  }
}
