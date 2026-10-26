import { computed, ref } from 'vue'
import type { useReaderExtrasStore } from '../stores/readerExtras'

type ReaderExtrasStore = ReturnType<typeof useReaderExtrasStore>

/// 评论面板粘合：章评/段评两个入口共用一个面板，状态就是
/// 「开关 + 模式 + 段号」；段文本与作者标记从 extras store 的段评概览里查。
export function useCommentPanelGlue(extrasStore: ReaderExtrasStore) {
  const showCommentPanel = ref(false)
  const commentMode = ref<'chapter' | 'para'>('chapter')
  const commentParaIndex = ref(0)
  const commentParaText = computed(
    () => extrasStore.paraReviewCountByIndex.get(commentParaIndex.value)?.text || ''
  )
  // 概览没标记 authorCommented 的段落，作者扫描注定空手，「只看作者」入口不显示
  const commentParaAuthorMarked = computed(
    () => extrasStore.paraReviewCountByIndex.get(commentParaIndex.value)?.authorCommented === true
  )

  function openChapterComments() {
    commentMode.value = 'chapter'
    showCommentPanel.value = true
  }

  function openParaComments(paraIndex: number) {
    commentMode.value = 'para'
    commentParaIndex.value = paraIndex
    showCommentPanel.value = true
  }

  function closeCommentPanel() {
    showCommentPanel.value = false
  }

  return {
    showCommentPanel,
    commentMode,
    commentParaIndex,
    commentParaText,
    commentParaAuthorMarked,
    openChapterComments,
    openParaComments,
    closeCommentPanel,
  }
}
