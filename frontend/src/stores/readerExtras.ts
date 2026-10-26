// 评论（章评/段评）与章节配图：从 reader store 拆出的子 store（reader store 拆分专项）。
// 两者都是「附加内容」：书源没声明规则时 enabled:false 不渲染入口，失败静默降级，
// 不能影响正文阅读。
//
// 依赖方向：本 store 依赖 reader 核心（stores/reader）；reader 不反向依赖本 store。
// 章节配图由章节激活驱动：readerContent 监听核心的 chapterActivation 信号后调
// 本 store 的 loadChapterImages（等价于拆分前 setActiveChapterState 内的直接触发）；
// 评论由各加载路径显式调 loadChapterReviews（refresh 语义只有调用方知道）。

import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { useReaderStore } from './reader'
import { getChapterImages } from '../api/bookshelf'
import { getChapterComments, getParaCommentIndex } from '../api/review'
import type { ChapterImage, ParaReviewCount, ReviewPage } from '../types'
import { isLocalBook } from '../utils/localBook'

export const useReaderExtrasStore = defineStore('readerExtras', () => {
  const readerStore = useReaderStore()

  /* 评论（章评 / 段评）：由书源规则决定是否可用 */
  const reviewEnabled = ref(false)
  const paraReviewEnabled = ref(false)
  const reviewAuthorMarks = ref(false)
  const paraReviewIndex = ref<ParaReviewCount[]>([])
  const chapterCommentTotal = ref(0)
  const chapterComments = ref<ReviewPage | null>(null)
  const reviewsLoading = ref(false)
  /* 章节配图：书源声明了配图规则时才有 */
  const chapterImages = ref<ChapterImage[]>([])
  const chapterImagesEnabled = ref(false)

  /* 书源级「不支持」结论：后端 enabled:false 只取决于书源声明没声明规则，
     与章节无关。同一本书记住后翻章不再重复请求（loadBook 时重置，换书/换源
     重新判断；显式刷新正文也会重新判断）。取舍：书源规则改了，要刷新本章
     或重开这本书才生效，不做自动轮询 */
  let paraReviewsDisabledBySource = false
  let chapterReviewsDisabledBySource = false
  let chapterImagesDisabledBySource = false

  /** 换书/换源：书源级「不支持」结论全部失效，重新判断（readerContent.loadBook 调） */
  function resetChapterSupportFlags() {
    paraReviewsDisabledBySource = false
    chapterReviewsDisabledBySource = false
    chapterImagesDisabledBySource = false
  }

  /** 显式刷新正文前调用：绕过「书源不支持配图」的缓存结论重新判断（书源可能刚补了规则） */
  function reprobeChapterImages() {
    chapterImagesDisabledBySource = false
  }

  /**
   * 拉取当前章节的段评概览与章评第一页。
   *
   * 书源没声明评论规则时后端返回 `enabled: false`，此时前端不渲染任何入口；
   * 网络或解析失败同样静默降级——评论是附加内容，不能让它影响正文阅读。
   */
  async function loadChapterReviews(index = readerStore.currentIndex, refresh = false) {
    const currentBook = readerStore.book
    const chapter = readerStore.chapters[index]
    if (!currentBook || !chapter || isLocalBook(currentBook)) {
      resetReviews()
      return
    }
    // 只认自己那次请求的结果，避免快速翻章时旧响应覆盖新章节
    const requestedChapterUrl = chapter.url
    const skipParaReviews = !refresh && paraReviewsDisabledBySource
    const skipChapterReviews = !refresh && chapterReviewsDisabledBySource
    // 两条规则都不支持：不再发任何请求，同时清掉上一章残留的评论
    if (skipParaReviews && skipChapterReviews) {
      resetReviews()
      return
    }
    reviewsLoading.value = true
    try {
      if (!currentBook.bookId) throw new Error('书籍缺少 bookId')
      const effectiveKind = readerStore.chapterVersions.get(index)?.kind
      const params = {
        bookId: currentBook.bookId,
        chapterUrl: requestedChapterUrl,
        // 手动刷新正文时让段评索引也重抓：两者必须来自同一版上游，
        // 否则段号会和刷新后的正文对不上
        refresh: refresh ? 1 : 0,
        // 段评定位看的是实际显示的版本：用户版本与上游版本的段号可能不同
        variant:
          effectiveKind === 'upstream'
            ? ('upstream' as const)
            : effectiveKind
              ? ('edited' as const)
              : readerStore.config.contentVersion,
      }
      // 段评与章评是两条独立规则：只禁用其一时，另一个仍然请求
      if (skipParaReviews) {
        paraReviewEnabled.value = false
        paraReviewIndex.value = []
      } else {
        // 先取段评概览：后面的章评请求能复用这次取回的章节正文响应
        const indexResp = await getParaCommentIndex(params)
        if (readerStore.chapters[readerStore.currentIndex]?.url !== requestedChapterUrl) return
        paraReviewsDisabledBySource = indexResp.enabled !== true
        paraReviewEnabled.value = indexResp.enabled
        paraReviewIndex.value = indexResp.data?.paras || []
      }
      if (skipChapterReviews) {
        reviewEnabled.value = false
        reviewAuthorMarks.value = false
        chapterCommentTotal.value = 0
        chapterComments.value = null
      } else {
        const chapterResp = await getChapterComments({ ...params, page: 1 })
        if (readerStore.chapters[readerStore.currentIndex]?.url !== requestedChapterUrl) return
        chapterReviewsDisabledBySource = chapterResp.enabled !== true
        reviewEnabled.value = chapterResp.enabled
        reviewAuthorMarks.value = chapterResp.authorMarks === true
        chapterCommentTotal.value = chapterResp.data?.total || 0
        chapterComments.value = chapterResp.data || null
      }
    } catch {
      if (readerStore.chapters[readerStore.currentIndex]?.url === requestedChapterUrl)
        resetReviews()
    } finally {
      reviewsLoading.value = false
    }
  }

  function resetReviews() {
    reviewEnabled.value = false
    paraReviewEnabled.value = false
    reviewAuthorMarks.value = false
    paraReviewIndex.value = []
    chapterCommentTotal.value = 0
    chapterComments.value = null
  }

  /**
   * 拉取本章配图。
   *
   * 与评论同理：书源没声明配图规则时后端返回 `enabled: false`，前端不渲染配图区；
   * 失败静默降级——配图是附加内容，不能影响正文阅读。
   * 后端不缓存配图（图片地址带时效签名），所以这里也不做本地缓存。
   */
  async function loadChapterImages(index = readerStore.currentIndex) {
    const currentBook = readerStore.book
    const chapter = readerStore.chapters[index]
    if (!currentBook || !chapter || isLocalBook(currentBook)) {
      resetChapterImages()
      return
    }
    // 只认自己那次请求的结果，避免快速翻章时旧响应覆盖新章节
    const requestedChapterUrl = chapter.url
    // 书源没有配图规则是书源级结论（见 loadChapterReviews 上方的标记说明）：
    // 记住后本书翻章不再请求，只清掉上一章的配图避免残留在新章上
    if (chapterImagesDisabledBySource) {
      resetChapterImages()
      return
    }
    try {
      if (!currentBook.bookId) throw new Error('书籍缺少 bookId')
      const resp = await getChapterImages({
        bookId: currentBook.bookId,
        chapterUrl: requestedChapterUrl,
      })
      if (readerStore.chapters[readerStore.currentIndex]?.url !== requestedChapterUrl) return
      chapterImagesEnabled.value = resp?.enabled === true
      chapterImages.value = resp?.images || []
      if (resp?.enabled !== true) chapterImagesDisabledBySource = true
    } catch {
      if (readerStore.chapters[readerStore.currentIndex]?.url === requestedChapterUrl)
        resetChapterImages()
    }
  }

  function resetChapterImages() {
    chapterImagesEnabled.value = false
    chapterImages.value = []
  }

  /** 段号 → 该段评论条数，供正文渲染气泡时查表。 */
  const paraReviewCountByIndex = computed(() => {
    const map = new Map<number, ParaReviewCount>()
    if (!paraReviewEnabled.value) return map
    for (const item of paraReviewIndex.value) map.set(item.paraIndex, item)
    return map
  })

  return {
    reviewEnabled,
    paraReviewEnabled,
    reviewAuthorMarks,
    paraReviewIndex,
    paraReviewCountByIndex,
    chapterCommentTotal,
    chapterComments,
    reviewsLoading,
    loadChapterReviews,
    resetReviews,
    chapterImages,
    chapterImagesEnabled,
    loadChapterImages,
    resetChapterImages,
    resetChapterSupportFlags,
    reprobeChapterImages,
  }
})
