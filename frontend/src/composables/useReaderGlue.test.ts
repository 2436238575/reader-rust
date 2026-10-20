// @vitest-environment jsdom
import { ref } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import { useCommentPanelGlue } from './useCommentPanelGlue'
import { useOverlayHistory } from './useOverlayHistory'
import { useReaderKeyboard } from './useReaderKeyboard'

function key(keyName: string) {
  return new KeyboardEvent('keydown', { key: keyName })
}

describe('useCommentPanelGlue', () => {
  function makeStore() {
    return {
      paraReviewCountByIndex: new Map([
        [3, { text: '第三段原文', count: 5, authorCommented: true }],
        [7, { text: '第七段原文', count: 2, authorCommented: false }],
      ]),
    }
  }

  it('章评/段评入口共用一个面板，段号与文本从概览查', () => {
    const glue = useCommentPanelGlue(makeStore() as never)
    glue.openChapterComments()
    expect(glue.showCommentPanel.value).toBe(true)
    expect(glue.commentMode.value).toBe('chapter')

    glue.openParaComments(3)
    expect(glue.commentMode.value).toBe('para')
    expect(glue.commentParaIndex.value).toBe(3)
    expect(glue.commentParaText.value).toBe('第三段原文')
    expect(glue.commentParaAuthorMarked.value).toBe(true)

    // 概览没标记作者的段落不露出「只看作者」入口
    glue.openParaComments(7)
    expect(glue.commentParaAuthorMarked.value).toBe(false)
    // 概览里没有的段号：空文本不炸
    glue.openParaComments(99)
    expect(glue.commentParaText.value).toBe('')

    glue.closeCommentPanel()
    expect(glue.showCommentPanel.value).toBe(false)
  })
})

describe('useOverlayHistory', () => {
  it('closeTopmostOverlayPanel 按 panels 顺序关最上层', () => {
    const a = ref(false)
    const b = ref(false)
    const closed: string[] = []
    const history = useOverlayHistory([
      { isOpen: a, close: () => closed.push('a') },
      { isOpen: b, close: () => closed.push('b') },
    ])
    history.closeTopmostOverlayPanel()
    expect(closed).toEqual([]) // 都没开

    b.value = true
    history.closeTopmostOverlayPanel()
    expect(closed).toEqual(['b'])

    a.value = true
    b.value = true
    history.closeTopmostOverlayPanel()
    expect(closed).toEqual(['b', 'a']) // a 优先级更高，先关 a
  })

  it('面板打开时压入历史标记', async () => {
    const a = ref(false)
    const pushSpy = vi.spyOn(history, 'pushState')
    useOverlayHistory([{ isOpen: a, close: () => (a.value = false) }])
    a.value = true
    await Promise.resolve()
    expect(pushSpy).toHaveBeenCalled()
    pushSpy.mockRestore()
  })
})

describe('useReaderKeyboard', () => {
  function makeHarness(overrides: Record<string, unknown> = {}) {
    const calls: string[] = []
    const panelA = ref(false)
    const panelB = ref(false)
    const store = { isAutoScrolling: false }
    const appStore = { showSettingsDrawer: false, showCacheLibrary: false }
    const { handleKeydown } = useReaderKeyboard(
      store as never,
      appStore as never,
      {
        escChain: [
          { isOpen: () => panelA.value, close: () => ((panelA.value = false), calls.push('closeA')) },
          { isOpen: () => panelB.value, close: () => ((panelB.value = false), calls.push('closeB')) },
        ],
        isBlockingOverlayOpen: () => panelA.value || panelB.value,
        isControlsVisible: () => false,
        hideControls: () => calls.push('hideControls'),
      },
      {
        goBack: () => calls.push('goBack'),
        pageForward: () => calls.push('pageForward'),
        pageBackward: () => calls.push('pageBackward'),
        nextChapter: () => calls.push('nextChapter'),
        prevChapter: () => calls.push('prevChapter'),
        scrollToTop: () => calls.push('scrollToTop'),
        scrollToBottom: () => calls.push('scrollToBottom'),
        scrollPageVertical: (d: number) => calls.push(`scrollV${d}`),
        isHorizontalPageMode: () => false,
        hasScrollContainer: () => true,
        interruptAutoScroll: () => calls.push('interruptAutoScroll'),
        ...overrides,
      }
    )
    return { calls, panelA, panelB, handleKeydown, store, appStore }
  }

  it('ESC 按链序关最上层面板', () => {
    const { calls, panelA, panelB, handleKeydown } = makeHarness()
    panelB.value = true
    handleKeydown(key('Escape'))
    expect(calls).toEqual(['closeB'])
    panelA.value = true
    panelB.value = true
    handleKeydown(key('Escape'))
    expect(calls).toEqual(['closeB', 'closeA']) // A 优先级更高
  })

  it('无面板时 ESC 落到 goBack；App 级弹窗打开时不穿透', () => {
    const { calls, handleKeydown, appStore } = makeHarness()
    handleKeydown(key('Escape'))
    expect(calls).toEqual(['goBack'])
    calls.length = 0
    appStore.showSettingsDrawer = true
    handleKeydown(key('Escape'))
    expect(calls).toEqual([])
  })

  it('输入框聚焦时非 ESC 键不翻页', () => {
    const { calls, handleKeydown } = makeHarness()
    const input = document.createElement('input')
    document.body.appendChild(input)
    input.focus()
    handleKeydown(key('PageDown'))
    expect(calls).toEqual([])
    input.blur()
    document.body.removeChild(input)
  })

  it('竖滚模式 PageDown 走 0.8 屏滚动，横翻模式走 pageForward', () => {
    const vertical = makeHarness()
    vertical.handleKeydown(key('PageDown'))
    expect(vertical.calls).toEqual(['scrollV1'])

    const horizontal = makeHarness({ isHorizontalPageMode: () => true })
    horizontal.handleKeydown(key('PageDown'))
    expect(horizontal.calls).toEqual(['pageForward'])
  })

  it('覆盖型面板打开时翻页键不穿透', () => {
    const { calls, panelA, handleKeydown } = makeHarness()
    panelA.value = true
    handleKeydown(key('PageDown'))
    handleKeydown(key('ArrowRight'))
    expect(calls).toEqual([])
  })

  it('自动滚动中按翻页键先接管再翻页', () => {
    const { calls, handleKeydown, store } = makeHarness()
    store.isAutoScrolling = true
    handleKeydown(key('PageDown'))
    expect(calls).toEqual(['interruptAutoScroll', 'scrollV1'])
  })

  it('ArrowRight/ArrowLeft 直接跳章（横翻竖滚一致）', () => {
    const { calls, handleKeydown } = makeHarness()
    handleKeydown(key('ArrowRight'))
    handleKeydown(key('ArrowLeft'))
    expect(calls).toEqual(['nextChapter', 'prevChapter'])
  })
})
