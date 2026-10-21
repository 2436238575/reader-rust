import { ref } from 'vue'
import type { ComputedRef, Ref } from 'vue'
import type { useReaderStore } from '../stores/reader'
import type { useAppStore } from '../stores/app'
import { saveReplaceRule } from '../api/replaceRule'
import { progressToChapterPos } from '../utils/bookmarks'
import { DEFAULT_KEYWORD_COLOR, newKeywordRuleId } from '../utils/keywordHighlights'

type ReaderStore = ReturnType<typeof useReaderStore>
type AppStore = ReturnType<typeof useAppStore>

export function useReaderSelection(
  store: ReaderStore,
  appStore: AppStore,
  config: ComputedRef<{ selectAction: 'popup' | 'contextmenu' | 'ignore' }>,
  scrollContainerRef: Ref<HTMLElement | undefined>
) {
  const isTouchDevice =
    typeof window !== 'undefined' && ('ontouchstart' in window || navigator.maxTouchPoints > 0)
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
    // 触屏 2(Android)/4(iOS) 字符下限；桌面端鼠标容易在点击间隙误划出
    // 1 字选区，也给个下限
    if (isTouchDevice && text.length < (isAndroid ? 2 : 4)) {
      hideSelectionMenu()
      return
    }
    if (!isTouchDevice && text.length < 2) {
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
    const targetNode =
      commonAncestor.nodeType === Node.TEXT_NODE
        ? commonAncestor.parentElement
        : (commonAncestor as HTMLElement | null)
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
        ? Math.min(
            window.innerHeight - 76,
            Math.max(
              16 +
                Math.max(
                  0,
                  parseFloat(
                    getComputedStyle(document.documentElement).getPropertyValue('--safe-area-top')
                  ) || 0
                ),
              rect.bottom + 12
            )
          )
        : Math.max(
            16 +
              Math.max(
                0,
                parseFloat(
                  getComputedStyle(document.documentElement).getPropertyValue('--safe-area-top')
                ) || 0
              ),
            rect.top - 56
          ),
      left: Math.min(window.innerWidth - 240, Math.max(16, rect.left + rect.width / 2 - 110)),
    }
  }

  /**
   * 划线锚点（万分比章内进度 0–10000）：优先按选区在文档流里的真实位置换算，
   * 换算不了退化为当前阅读进度。横翻容器 overflow-y 隐藏、scrollTop 恒 0，
   * 直接用分页器维护的章内进度（store.chapterScrollProgress 三模式都是章内语义）。
   * 连续模式下选区可能落在非当前章，把所在章的 chapterIndex 一并带回。
   */
  function computeSelectionAnchor(): { chapterPos: number; chapterIndex?: number } {
    const fallback = progressToChapterPos(store.chapterScrollProgress)
    const container = scrollContainerRef.value
    const selection = window.getSelection?.()
    if (!container || !selection || selection.rangeCount === 0) return { chapterPos: fallback }
    const range = selection.getRangeAt(0)
    const rect = range.getBoundingClientRect()
    if (!rect.width && !rect.height) return { chapterPos: fallback }

    const containerRect = container.getBoundingClientRect()
    // 选区中心在容器内容流里的绝对纵坐标
    const center = container.scrollTop + (rect.top - containerRect.top) + rect.height / 2

    // 连续模式：选区所在的章 section 直接给章内进度与章号
    const startEl =
      range.startContainer.nodeType === Node.TEXT_NODE
        ? range.startContainer.parentElement
        : (range.startContainer as HTMLElement | null)
    const sectionEl = startEl?.closest?.('section.continuous-chapter') as HTMLElement | null
    if (sectionEl?.dataset.chapterIndex != null) {
      const chapterIndex = Number(sectionEl.dataset.chapterIndex)
      const sectionRange = Math.max(1, sectionEl.offsetHeight)
      const progress = Math.max(0, Math.min(1, (center - sectionEl.offsetTop) / sectionRange))
      return {
        chapterPos: progressToChapterPos(progress),
        chapterIndex: Number.isFinite(chapterIndex) ? chapterIndex : undefined,
      }
    }

    // 竖滚单章：容器可滚动范围内按选区中心换算；横翻 scrollable<=0 走 fallback
    const scrollable = container.scrollHeight - container.clientHeight
    if (scrollable <= 0) return { chapterPos: fallback }
    return { chapterPos: progressToChapterPos(center / scrollable) }
  }

  async function addSelectionBookmark() {
    const selection = window.getSelection?.()
    const text = activeSelectionText.value || selection?.toString().trim() || ''
    if (!text) return
    try {
      const anchor = computeSelectionAnchor()
      await store.addBookmark(anchor.chapterPos, text, 'underline', anchor.chapterIndex)
      selection?.removeAllRanges()
      activeSelectionText.value = ''
      hideSelectionMenu()
      appStore.showToast('已加入划线', 'success')
    } catch {
      appStore.showToast('加入划线失败', 'error')
    }
  }

  async function addSelectionReplaceRule(mode: 'book' | 'source') {
    const selection = window.getSelection?.()
    const text = activeSelectionText.value || selection?.toString().trim() || ''
    if (!text || !store.book) return

    try {
      const scope =
        mode === 'source' ? `source:${store.book.bookSourceId}` : `book:${store.book.bookId}`
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

  /** 把文本加为常驻关键词高亮（默认当前书）；选区与划线菜单共用 */
  function addKeywordHighlightForText(rawText: string): boolean {
    const text = rawText.trim()
    if (!text || !store.book) return false
    const bookId = store.book.bookId
    const exists = store.keywordRules.some(
      (rule) =>
        rule.keyword === text &&
        (rule.scope === 'global' || (rule.scope === 'book' && rule.bookId === bookId))
    )
    if (exists) {
      appStore.showToast('该词已在高亮列表中', 'warning')
      return false
    }
    store.saveKeywordRules([
      ...store.keywordRules,
      {
        id: newKeywordRuleId(),
        keyword: text,
        color: DEFAULT_KEYWORD_COLOR,
        enabled: true,
        scope: 'book',
        bookId,
        updatedAt: Date.now(),
      },
    ])
    return true
  }

  /** 按文本加替换规则；选区与划线菜单共用 */
  async function saveReplaceRuleForText(text: string, mode: 'book' | 'source') {
    if (!store.book) return
    const scope =
      mode === 'source' ? `source:${store.book.bookSourceId}` : `book:${store.book.bookId}`
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
  }

  /** 把选中文本加为常驻关键词高亮（默认当前书） */
  function addSelectionKeywordHighlight() {
    const selection = window.getSelection?.()
    const text = (activeSelectionText.value || selection?.toString().trim() || '').trim()
    if (!text || !store.book) return
    if (!addKeywordHighlightForText(text)) return
    selection?.removeAllRanges()
    activeSelectionText.value = ''
    hideSelectionMenu()
    appStore.showToast('已加入高亮（当前书籍）', 'success')
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
    const targetNode =
      commonAncestor.nodeType === Node.TEXT_NODE
        ? commonAncestor.parentElement
        : (commonAncestor as HTMLElement | null)
    if (!container || !targetNode || !container.contains(targetNode)) {
      return false
    }
    // 桌面右键是显式动作，不套触摸端的最短选区长度门槛
    suppressSelectionCloseUntil.value = Date.now() + 250
    activeSelectionText.value = text
    // 像原生右键菜单：以鼠标位置为左上角，视口内夹紧（菜单约 4 行高）
    const safeTop =
      8 +
      Math.max(
        0,
        parseFloat(
          getComputedStyle(document.documentElement).getPropertyValue('--safe-area-top')
        ) || 0
      )
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

  /* ─── 已回显划线的操作菜单（点击/右键/长按打开） ─── */
  const underlineMenu = ref({
    visible: false,
    /** 被点那一段的文本（高亮/替换动作用；跨段划线的其余段不混进来） */
    text: '',
    top: 0,
    left: 0,
    chapterIndex: 0,
    chapterPos: 0,
  })

  function openUnderlineMenu(
    x: number,
    y: number,
    text: string,
    chapterIndex: number,
    chapterPos: number
  ) {
    hideSelectionMenu()
    suppressSelectionCloseUntil.value = Date.now() + 250
    const safeTop =
      8 +
      Math.max(
        0,
        parseFloat(
          getComputedStyle(document.documentElement).getPropertyValue('--safe-area-top')
        ) || 0
      )
    underlineMenu.value = {
      visible: true,
      text,
      // 菜单约 4 行高、240px 宽，钳在视口内
      top: Math.max(safeTop, Math.min(window.innerHeight - 190, y + 12)),
      left: Math.max(8, Math.min(window.innerWidth - 240, x - 110)),
      chapterIndex,
      chapterPos,
    }
  }

  function closeUnderlineMenu() {
    underlineMenu.value.visible = false
  }

  /** 划线菜单的删除：按 (chapterIndex, chapterPos, kind) 定位回书签条目 */
  async function deleteUnderlineFromMenu() {
    const { chapterIndex, chapterPos } = underlineMenu.value
    const bm = store.bookmarks.find(
      (b) =>
        b.kind === 'underline' &&
        b.chapterIndex === chapterIndex &&
        (b.chapterPos ?? 0) === chapterPos
    )
    if (!bm) {
      closeUnderlineMenu()
      return
    }
    if (!confirm('确定删除这条划线吗？')) return
    try {
      await store.removeBookmark(bm)
      closeUnderlineMenu()
      appStore.showToast('已删除划线', 'success')
    } catch (error) {
      appStore.showToast((error as Error).message || '删除划线失败', 'error')
    }
  }

  function underlineMenuAddHighlight() {
    if (addKeywordHighlightForText(underlineMenu.value.text)) {
      appStore.showToast('已加入高亮（当前书籍）', 'success')
    }
    closeUnderlineMenu()
  }

  async function underlineMenuAddReplaceRule(mode: 'book' | 'source') {
    try {
      await saveReplaceRuleForText(underlineMenu.value.text, mode)
      appStore.showToast(mode === 'source' ? '已加入书源替换规则' : '已加入本书替换规则', 'success')
    } catch {
      appStore.showToast('加入替换规则失败', 'error')
    }
    closeUnderlineMenu()
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
    addSelectionKeywordHighlight,
    underlineMenu,
    openUnderlineMenu,
    closeUnderlineMenu,
    deleteUnderlineFromMenu,
    underlineMenuAddHighlight,
    underlineMenuAddReplaceRule,
    clearSelectionState,
    disposeSelection,
  }
}
