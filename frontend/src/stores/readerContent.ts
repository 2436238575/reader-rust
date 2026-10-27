// 正文读取与章节编排：从 reader store 拆出的子 store（reader store 拆分专项）。
// 负责：开书/会话恢复/清空、单章与批量正文读取（含浏览器缓存兜底与版本/diff 落账）、
// 预加载、换源、显式刷新与版本切换、上一章/下一章。
//
// 依赖方向：本 store 依赖 reader 核心（stores/reader）与 app/bookshelf/aiBook；
// stores/reader 不反向依赖本 store。评论/配图、已读标记等在本提交仍是 reader 成员，
// 随后续拆分提交迁出后改接对应子 store。

import { defineStore } from 'pinia'
import { ref, watch } from 'vue'
import { useAppStore } from './app'
import { useBookshelfStore } from './bookshelf'
import { useAiBookStore } from './aiBook'
import { useReaderStore } from './reader'
import { useReaderExtrasStore } from './readerExtras'
import { useReaderMarksStore } from './readerMarks'
import { useReaderConfigStore } from './readerConfig'
import {
  getChapterList,
  getBookContent,
  getBookContents,
  setBookSource as apiSetBookSource,
} from '../api/bookshelf'
import type { ChapterContentResponse, ContentVariant } from '../api/bookshelf'
import { selectChapterVersion } from '../api/chapterEdit'
import type { Book, BookChapter } from '../types'
import {
  getBrowserCachedChapter,
  getBrowserCatalog,
  setBrowserCachedChapter,
  setBrowserCatalog,
} from '../utils/browserCache'
import { isLocalBook } from '../utils/localBook'
import { describeRequestFailure } from '../utils/requestFailure'
import { hydrateDiffOps } from '../utils/chapterDiff'
import { loadCachedEditedChapterKeys } from '../utils/editedChaptersCache'

export const useReaderContentStore = defineStore('readerContent', () => {
  const readerStore = useReaderStore()
  const extrasStore = useReaderExtrasStore()
  const marksStore = useReaderMarksStore()
  const configStore = useReaderConfigStore()
  const appStore = useAppStore()
  const shelfStore = useBookshelfStore()
  const aiBookStore = useAiBookStore()

  // 配图跟着章节激活走：先清掉上一章的，再按新正文重新取。
  // 监听激活信号而不是散在各调用方：换章、切正文版本、刷新、连续滚动激活
  // 都会经过 setActiveChapterState，漏一处就会少一次配图请求
  watch(
    () => readerStore.chapterActivation,
    () => {
      extrasStore.resetChapterImages()
      void extrasStore.loadChapterImages(readerStore.currentIndex)
    }
  )

  /* ─── Book / chapter ops ─── */
  let bookLoadSeq = 0
  let chapterLoadSeq = 0
  // 加载失败的目标章：重试时要重拉的是它，而不是还停留在旧章的 currentIndex
  const failedChapterIndex = ref<number | null>(null)

  async function loadBook(b: Book) {
    // 连点两本书时目录响应可能乱序到达：只认最后一次打开的书。
    // 同时作废在途的 loadChapter，免得旧书正文写到新书上。
    const loadSeq = ++bookLoadSeq
    chapterLoadSeq++
    readerStore.loading = true
    readerStore.loadError = ''
    readerStore.book = b
    // 打开这本书时服务端进度的真实保存时间。setActiveChapterState 会把
    // book.durChapterTime 刷成 Date.now()（书架排序需要），本地/服务端进度
    // 比新旧时若直接用被刷新的值，服务端会靠这个伪造的新鲜时间戳「永远获胜」，
    // 离线读了几章没同步成功的本地进度就被误判为旧数据丢弃
    readerStore.serverProgressTime = b.durChapterTime || 0
    readerStore.chapters = []
    readerStore.content = ''
    extrasStore.resetReviews()
    // 先用上次成功拉取时落盘的快照：离线开书时清单拉不到，编辑按钮的
    // 状态靠快照维持。在线时下面的 refreshEditedChapterKeys 会用服务端
    // 结果覆盖并更新快照
    readerStore.editedChapterKeys = new Set(
      b.bookId ? loadCachedEditedChapterKeys(b.bookId) : []
    )
    // 书源级「不支持评论/配图」结论也按书失效：换书/换源后重新判断
    extrasStore.resetChapterSupportFlags()
    appStore.markBookOpened(b.bookId || b.bookUrl)
    readerStore.currentIndex = b.durChapterIndex || 0
    readerStore.chapterScrollProgress = 0
    readerStore.preloadedContent.clear()
    readerStore.chapterDiffs.clear()
    marksStore.loadReadChapterHistory(b)
    readerStore.progressDirty = false
    readerStore.lastServerProgressKey = ''
    readerStore.chaptersLoading = true
    try {
      if (!b.bookId) throw new Error('书籍缺少 bookId')
      // 目录与编辑章清单并行；loadBook 返回前两者都要落定，否则开书后的
      // 正文请求可能赶在集合就位前走浏览器缓存/写回旧缓存。
      // refreshEditedChapterKeys 内部已吞异常，不会让 Promise.all reject。
      // 离线前置：浏览器已报离线时目录请求必然失败，最坏还要白等一个超时
      // 周期才走兜底——直接拒绝进 catch 走浏览器缓存（catch 里
      //  describeRequestFailure 对离线恒返回「离线中」，提示口径不变）
      const [list] = await Promise.all([
        appStore.isOnline
          ? getChapterList({ bookId: b.bookId })
          : Promise.reject(new Error('离线中')),
        readerStore.refreshEditedChapterKeys(),
      ])
      if (loadSeq !== bookLoadSeq) return
      readerStore.chapters = list
      // 目录落浏览器缓存：离线时同一本书还能打开
      void setBrowserCatalog(b.bookId, list)
      readerStore.saveReaderSession()
    } catch (error) {
      // 目录都拿不到时先试浏览器里的目录缓存（离线场景），
      // 实在没有才等于这本书打不开。清掉半开状态并记下原因：否则阅读页
      // 会一直停在「加载中...」的占位符上，用户完全看不出发生了什么。
      const cachedCatalog = b.bookId ? await getBrowserCatalog<BookChapter>(b.bookId) : null
      if (cachedCatalog && loadSeq === bookLoadSeq) {
        readerStore.chapters = cachedCatalog
        readerStore.chaptersLoading = false
        // 原因要如实：后端在线但上游挂了（502）与浏览器离线是两回事
        appStore.showToast(
          `${describeRequestFailure(error, appStore.isOnline)}：目录来自浏览器缓存`,
          'warning'
        )
        return
      }
      if (loadSeq === bookLoadSeq) {
        readerStore.loading = false
        readerStore.chapters = []
        readerStore.content = ''
        readerStore.loadError = describeRequestFailure(error, appStore.isOnline)
        appStore.showToast(readerStore.loadError, 'error')
      }
      throw error
    } finally {
      if (loadSeq === bookLoadSeq) readerStore.chaptersLoading = false
    }
  }

  async function restorePersistedSession() {
    const session = readerStore.getPersistedReaderSession()
    if (!session?.book) return false

    readerStore.book = session.book
    readerStore.chapters = session.chapters ?? []
    // 瘦身会话（配额降级时落的盘）不带目录：恢复时重新拉一次
    if (!readerStore.chapters.length) {
      const bookId = session.book.bookId
      if (!bookId) return false
      try {
        // 离线前置：目录请求必然失败且要等超时，直接进 catch 走浏览器缓存目录
        if (!appStore.isOnline) throw new Error('离线中')
        const list = await getChapterList({ bookId })
        readerStore.chapters = list
        void setBrowserCatalog(bookId, list)
      } catch (error) {
        // 离线兜底：浏览器里缓存的目录
        const cachedCatalog = await getBrowserCatalog<BookChapter>(bookId)
        if (!cachedCatalog) return false
        readerStore.chapters = cachedCatalog
        // 离线跳过网络时如实提示（与 loadBook 同一口径）；在线失败的静默兜底
        // 维持原样——恢复成功的 toast 随后就到，不叠一条 warning
        if (!appStore.isOnline) {
          appStore.showToast(
            `${describeRequestFailure(error, appStore.isOnline)}：目录来自浏览器缓存`,
            'warning'
          )
        }
      }
    }
    if (!readerStore.chapters.length) return false
    marksStore.loadReadChapterHistory(session.book)

    // 先拿到已编辑章清单再取正文：否则恢复会话时会走浏览器缓存，拿到的是
    // 编辑前的上游正文（diff 标记也随之消失）
    await readerStore.refreshEditedChapterKeys()

    const nextIndex = Math.max(
      0,
      Math.min(session.currentIndex || 0, readerStore.chapters.length - 1)
    )
    try {
      const chapterContent = await fetchChapterContent(nextIndex)
      if (chapterContent == null) return false
      readerStore.setActiveChapterState(
        nextIndex,
        chapterContent,
        session.chapterScrollProgress || 0
      )
      marksStore.markChapterAsRead(nextIndex)
      // 恢复会话不经过 loadChapter，评论要在这里单独补一次（配图由 setActive 触发）
      void extrasStore.loadChapterReviews(nextIndex)
      return true
    } catch {
      return false
    }
  }

  /**
   * 正文落账：版本信息、diff 标记与浏览器缓存写入。单章 fetchChapterContent 与
   * 批量预加载（preloadChaptersBatch）共用这一份，避免两条路径的版本记录与
   * 缓存行为各自漂移。
   */
  async function recordChapterContent(
    index: number,
    chapter: BookChapter,
    response: ChapterContentResponse,
    context: { bookId: string; isLocal: boolean; variant: ContentVariant }
  ) {
    readerStore.chapterVersions.set(index, {
      versionId: response.versionId,
      kind: response.versionKind,
      baseVersionId: response.baseVersionId,
      latestUserVersionId: response.latestUserVersionId,
      latestUpstreamVersionId: response.latestUpstreamVersionId,
      upstreamChanged: response.upstreamChanged,
      selectionSource: response.selectionSource,
      stale: response.stale,
    })
    if (response.edited) {
      // 后端为省流量不给 equal 行带文本，这里用编辑版正文补齐
      const ops = response.diff?.ops ? hydrateDiffOps(response.diff.ops, response.content) : null
      readerStore.chapterDiffs.set(index, {
        edited: true,
        ops,
        error: response.diffError || '',
        variant: response.variant,
      })
    } else {
      readerStore.chapterDiffs.delete(index)
    }

    // 上游预览不写浏览器缓存：缓存只存默认版本，否则离线时会读到另一版。
    // 编辑章与普通章一样写：记录里带版本号，串版本由在线读取时的比对兜底
    if (!context.isLocal && context.variant !== 'upstream') {
      await setBrowserCachedChapter({
        bookId: context.bookId,
        chapterUrl: chapter.url,
        chapterTitle: chapter.title,
        content: response.content,
        versionId: response.versionId,
      }).catch((error) => {
        // 写不进浏览器缓存（配额写满且无可淘汰）不该静默：阅读功能本身不受影响，
        // 但「缓存永远 0 章」这种症状没有日志就完全无法定位
        console.warn('[reader] 浏览器章节缓存写入失败', error)
      })
    }
  }

  async function fetchChapterContent(index: number, forceRefresh = false) {
    if (!readerStore.book?.bookId || !readerStore.chapters[index]) return null

    if (!forceRefresh && readerStore.preloadedContent.has(index)) {
      return readerStore.preloadedContent.get(index) || null
    }

    const chapter = readerStore.chapters[index]
    const bookId = readerStore.book.bookId

    const isLocal = isLocalBook(readerStore.book)
    const variant: ContentVariant =
      configStore.config.contentVersion === 'upstream' ? 'upstream' : 'default'
    // 浏览器缓存的两个角色：离线时的正文来源，在线时的版本比对凭证
    // （缓存记录带版本号，与后端当前版本一致就免传正文）。编辑章与普通章
    // 一视同仁——会不会串版本由版本号比对兜底，不再靠「编辑章不缓存」
    const cached = !isLocal
      ? await getBrowserCachedChapter(bookId, chapter.url).catch(() => null)
      : null

    if (!appStore.isOnline && !isLocal) {
      if (cached) {
        readerStore.chapterDiffs.delete(index)
        readerStore.chapterVersions.delete(index)
        return cached.content
      }
      throw new Error('当前处于离线状态，且该章节未缓存到浏览器')
    }

    let chapterContent = ''
    try {
      const response = await getBookContent({
        bookId,
        chapterUrl: chapter.url,
        index,
        refresh: forceRefresh ? 1 : 0,
        variant,
        // 强刷就是要新正文，不做版本比对
        knownVersionId: forceRefresh ? undefined : cached?.versionId,
      })
      if (response.unchanged) {
        if (cached) {
          // 服务端确认缓存版本仍是最新：正文直接用缓存，版本/diff 落账照常
          await recordChapterContent(
            index,
            chapter,
            { ...response, content: cached.content },
            { bookId, isLocal, variant }
          )
          return cached.content
        }
        // unchanged 却读不到本地缓存（并发清理等边缘情况）：重发一次不带
        // 版本号的请求拿全文，避免抱着空正文返回
        const full = await getBookContent({ bookId, chapterUrl: chapter.url, index, variant })
        chapterContent = full.content
        await recordChapterContent(index, chapter, full, { bookId, isLocal, variant })
      } else {
        chapterContent = response.content
        await recordChapterContent(index, chapter, response, { bookId, isLocal, variant })
      }
    } catch (error) {
      // 在线失败的缓存兜底：读到的就是缓存时点的版本，可能比服务端旧，
      // 恢复在线后的第一次读取会凭版本号比对自动换新
      if (cached) {
        readerStore.chapterDiffs.delete(index)
        readerStore.chapterVersions.delete(index)
        appStore.showToast(
          `${describeRequestFailure(error, appStore.isOnline)}，已切换到本地缓存章节`,
          'warning'
        )
        return cached.content
      }
      throw error
    }

    return chapterContent
  }

  async function loadChapter(index: number, forceRefresh = false) {
    if (!readerStore.book || !readerStore.chapters[index]) return

    // 快速连点下一章时旧响应可能后到：只认最新一次请求的结果
    const loadSeq = ++chapterLoadSeq
    readerStore.loading = true
    readerStore.loadError = ''
    failedChapterIndex.value = null
    try {
      const chapterContent = await fetchChapterContent(index, forceRefresh)
      if (chapterContent == null) return
      if (loadSeq !== chapterLoadSeq) return

      const currentBook = readerStore.book
      const previousSavedIndex = currentBook.durChapterIndex ?? 0
      const previousSavedProgress = readerStore.decodeServerProgress(currentBook.durChapterPos)
      const isOpeningSavedChapter = !forceRefresh && index === previousSavedIndex
      const initialProgress = isOpeningSavedChapter ? previousSavedProgress : 0

      readerStore.setActiveChapterState(index, chapterContent, initialProgress)
      marksStore.markChapterAsRead(index)
      appStore.markChapterRead(
        currentBook.bookId || currentBook.bookUrl,
        index,
        readerStore.chapters.length
      )

      if (!isOpeningSavedChapter) {
        await readerStore.persistProgress(index, 0)
      }

      // 正文先渲染，评论随后补上（配图由 setActiveChapterState 统一触发）
      void extrasStore.loadChapterReviews(index)

      if (configStore.config.enablePreload) {
        setTimeout(() => preloadAroundChapter(index), forceRefresh ? 1500 : 1000)
      }
    } catch (error) {
      // 失败要留下可见的错误态（正文区展示 + 重试入口），只清 loading 会让用户
      // 对着旧章或空白分不清「还在加载」还是「已经失败」。不再向外抛：所有
      // 调用方（翻章/目录/开书）都不接异常，抛出去只会变成 unhandled rejection。
      if (loadSeq === chapterLoadSeq) {
        readerStore.loadError = describeRequestFailure(error, appStore.isOnline)
        failedChapterIndex.value = index
        appStore.showToast(readerStore.loadError, 'error')
      }
    } finally {
      // 过期请求的 finally 不能灭掉新一轮加载的 loading
      if (loadSeq === chapterLoadSeq) readerStore.loading = false
    }
  }

  /** 失败重试：目录都没拿到就整书重载，否则重拉失败时目标章（无记录退回当前章） */
  async function retryLoad() {
    if (!readerStore.book) return
    try {
      if (!readerStore.chapters.length) {
        await loadBook(readerStore.book)
        await loadChapter(readerStore.currentIndex)
      } else {
        await loadChapter(failedChapterIndex.value ?? readerStore.currentIndex)
      }
    } catch {
      // loadBook 失败时已自行 toast，这里不再重复提示
    }
  }

  async function preloadAroundChapter(index: number) {
    if (!readerStore.book || !configStore.config.enablePreload) return
    const targets = [index + 1, index + 2, index - 1].filter(
      (target, pos, list) =>
        target >= 0 && target < readerStore.chapters.length && list.indexOf(target) === pos
    )
    // 批量接口一次取回全部目标章（后端上限 10，这里最多 3 条）；整批失败
    // （断网/401/400）时对全部目标章回退逐章预加载，行为与接入批量前一致；
    // 批量成功时只有失败章（单章 error / 响应缺失）逐章兜底——走单章接口
    // 重试并自带浏览器缓存兜底，成功章不重复请求
    const pending = targets.filter((target) => !readerStore.preloadedContent.has(target))
    if (pending.length > 0) {
      const batch = await preloadChaptersBatch(pending)
      if (batch.ok) {
        for (const target of batch.failed) {
          await preloadNextChapter(target)
        }
        return
      }
    }
    for (const target of targets) {
      await preloadNextChapter(target)
    }
  }

  /**
   * 批量预加载：一次 getBookContents 请求取回多章正文写入 preloadedContent。
   * 每章独立成败——`item.data` 为空或响应里缺失的章节记入 failed，由调用方
   * 逐章兜底（单章接口重试 + 浏览器缓存）；ok=false 表示整批失败，由调用方
   * 对全部目标章回退逐章 fetchChapterContent。
   */
  async function preloadChaptersBatch(
    targets: number[]
  ): Promise<{ ok: boolean; failed: number[] }> {
    const currentBook = readerStore.book
    if (!currentBook?.bookId) return { ok: false, failed: [] }
    const bookId = currentBook.bookId
    const isLocal = isLocalBook(currentBook)
    const variant: ContentVariant =
      configStore.config.contentVersion === 'upstream' ? 'upstream' : 'default'
    // 发送时的 index → chapterUrl 快照：响应回来时按它核对章节列表是否已变化
    const sentUrls = new Map<number, string>()
    // 各章浏览器缓存里的版本号：带回给后端比对，一致时该章不传正文
    const cachedByIndex = new Map<number, { content: string; versionId?: number }>()
    const chaptersPayload: { index: number; chapterUrl: string; knownVersionId?: number }[] = []
    for (const chapterIndex of targets) {
      const chapter = readerStore.chapters[chapterIndex]
      if (!chapter) continue
      sentUrls.set(chapterIndex, chapter.url)
      const cached = isLocal
        ? null
        : await getBrowserCachedChapter(bookId, chapter.url).catch(() => null)
      if (cached) cachedByIndex.set(chapterIndex, cached)
      chaptersPayload.push({
        index: chapterIndex,
        chapterUrl: chapter.url,
        knownVersionId: cached?.versionId,
      })
    }
    if (chaptersPayload.length === 0) return { ok: false, failed: [] }

    let items
    try {
      items = await getBookContents({ bookId, chapters: chaptersPayload, variant })
    } catch {
      return { ok: false, failed: [] }
    }
    if (!Array.isArray(items)) return { ok: false, failed: [] }
    // 响应期间换书/换源：旧请求的结果不能写进新书的预加载池（换书已清空
    // preloadedContent，这里直接丢弃，不算失败、也不再回退逐章）
    if (readerStore.book?.bookId !== bookId) return { ok: true, failed: [] }

    // 成功取回正文的请求章；其余（data 为空 / 响应缺失）都要逐章兜底
    const answered = new Set<number>()
    for (const item of items) {
      const chapterIndex = item?.index
      const sentUrl = typeof chapterIndex === 'number' ? sentUrls.get(chapterIndex) : undefined
      if (sentUrl === undefined) continue
      if (!item?.data) continue // 单章失败：原因在 error，记入 failed 走逐章兜底
      // unchanged：后端确认缓存版本仍是最新、没传正文，用本地缓存的正文；
      // 缓存恰好读不到（并发清理）时当失败处理，走逐章兜底重拉全文
      let data = item.data
      if (data.unchanged) {
        const cached = cachedByIndex.get(chapterIndex)
        if (!cached) continue
        data = { ...data, content: cached.content }
      }
      answered.add(chapterIndex)
      const chapter = readerStore.chapters[chapterIndex]
      if (!chapter || chapter.url !== sentUrl) continue
      if (readerStore.preloadedContent.has(chapterIndex)) continue
      // 与 preloadNextChapter 同一套容量约束：超出即淘汰最旧的一条
      if (readerStore.preloadedContent.size > 3) {
        const firstKey = readerStore.preloadedContent.keys().next().value
        if (firstKey !== undefined) readerStore.preloadedContent.delete(firstKey)
      }
      readerStore.preloadedContent.set(chapterIndex, data.content)
      // 版本信息/diff/浏览器缓存与单章路径同口径落账
      await recordChapterContent(chapterIndex, chapter, data, { bookId, isLocal, variant })
    }
    const failed = [...sentUrls.keys()].filter((chapterIndex) => !answered.has(chapterIndex))
    return { ok: true, failed }
  }

  async function preloadNextChapter(index: number) {
    if (
      !readerStore.book ||
      !configStore.config.enablePreload ||
      index >= readerStore.chapters.length ||
      readerStore.preloadedContent.has(index)
    )
      return

    // Keep max 3 preloaded chapters
    if (readerStore.preloadedContent.size > 3) {
      const firstKey = readerStore.preloadedContent.keys().next().value
      if (firstKey !== undefined) readerStore.preloadedContent.delete(firstKey)
    }

    try {
      const res = await fetchChapterContent(index)
      if (!res) return
      readerStore.preloadedContent.set(index, res)
    } catch {
      /* ignore */
    }
  }

  function normalizeChapterTitle(title?: string) {
    return (title || '')
      .replace(/\s+/g, '')
      .replace(/[^\p{L}\p{N}]/gu, '')
      .toLowerCase()
  }

  function resolveChapterIndexByTitle(
    list: BookChapter[],
    targetTitle?: string,
    fallbackIndex = 0
  ) {
    if (!list.length) return 0
    const normalizedTarget = normalizeChapterTitle(targetTitle)
    if (!normalizedTarget) {
      return Math.max(0, Math.min(list.length - 1, fallbackIndex))
    }

    const exactIndex = list.findIndex(
      (chapter) => normalizeChapterTitle(chapter.title) === normalizedTarget
    )
    if (exactIndex >= 0) return exactIndex

    const partialIndex = list.findIndex((chapter) => {
      const title = normalizeChapterTitle(chapter.title)
      return title.includes(normalizedTarget) || normalizedTarget.includes(title)
    })
    if (partialIndex >= 0) return partialIndex

    return Math.max(0, Math.min(list.length - 1, fallbackIndex))
  }

  /* ─── Switch Source ─── */
  /** 换源：目标是候选搜索结果自己的 bookId；当前书的 bookId 不变 */
  async function switchSource(targetBookId: string) {
    if (!readerStore.book?.bookId) return
    const previousChapterTitle =
      readerStore.currentChapter?.title || readerStore.book.durChapterTitle
    const previousIndex = readerStore.currentIndex
    const previousProgress = readerStore.chapterScrollProgress
    readerStore.loading = true
    try {
      const updatedBook = await apiSetBookSource({
        bookId: readerStore.book.bookId,
        targetBookId,
      })
      if (!updatedBook) return null

      await loadBook(updatedBook)
      const targetIndex = resolveChapterIndexByTitle(
        readerStore.chapters,
        previousChapterTitle,
        typeof updatedBook.durChapterIndex === 'number'
          ? updatedBook.durChapterIndex
          : previousIndex
      )
      await loadChapter(targetIndex)
      readerStore.setChapterScrollProgress(previousProgress)
      await shelfStore.fetchBooks().catch(() => undefined)
      return updatedBook
    } finally {
      readerStore.loading = false
    }
  }

  /** 批量导入后清掉全部章节的预加载与派生状态，后续翻章重新从后端取正文。
   *  只清当前章会让预加载过的相邻章继续显示导入前的旧正文。 */
  async function invalidateChapterContentCache() {
    readerStore.preloadedContent.clear()
    readerStore.chapterVersions.clear()
    readerStore.chapterDiffs.clear()
    await readerStore.refreshEditedChapterKeys()
  }

  /** 编辑版保存/删除后重载本章：清预载，重新走后端（编辑版优先），
   *  与 refreshContent 同构但不强刷上游。浏览器缓存不用先删：重载会拉到
   *  新版本并覆盖写入，旧记录顺便被更新。 */
  async function reloadChapterAfterEdit(index = readerStore.currentIndex) {
    const chapter = readerStore.chapters[index]
    if (!chapter || !readerStore.book?.bookId) return
    readerStore.preloadedContent.delete(index)
    await readerStore.refreshEditedChapterKeys()
    const loadSeq = ++chapterLoadSeq
    readerStore.loading = true
    try {
      const chapterContent = await fetchChapterContent(index, false)
      if (chapterContent == null) return
      if (loadSeq !== chapterLoadSeq || index !== readerStore.currentIndex) return
      readerStore.setActiveChapterState(index, chapterContent, readerStore.chapterScrollProgress)
      // 正文/版本变了，段评概览也要按当前版本重新取（服务端有缓存，代价很小）
      void extrasStore.loadChapterReviews(index)
    } finally {
      readerStore.loading = false
    }
  }

  /** 切换当前章显示版本并保存到服务端（跨设备生效）；`null` 表示跟随最新。 */
  async function setChapterVersion(index: number, versionId: number | null) {
    const chapter = readerStore.chapters[index]
    const bookId = readerStore.book?.bookId
    if (!chapter || !bookId) return
    await selectChapterVersion(bookId, chapter.url, versionId)
    readerStore.chapterVersions.delete(index)
    readerStore.preloadedContent.clear()
    await readerStore.refreshEditedChapterKeys()
    if (configStore.config.contentVersion === 'upstream') {
      // 在「上游原版」预览里选了版本：切回默认版本模式，ReaderView 的
      // contentVersion watcher 会用新的服务端选择重载本章。
      configStore.updateConfig('contentVersion', 'edited')
    } else {
      await reloadChapterAfterEdit(index)
    }
  }

  async function refreshContent() {
    if (!readerStore.book || !readerStore.chapters[readerStore.currentIndex]) return
    // 显式刷新绕过「书源不支持配图」的缓存结论重新判断：书源可能刚补了
    // 规则。reset 在 setActiveChapterState 之前，随后触发的 loadChapterImages
    // 就会真正发请求（评论走 loadChapterReviews(index, true)，自带重判）
    extrasStore.reprobeChapterImages()
    // 快照当前章节：await 期间用户可能已翻章，旧正文不能盖到新章上。
    // 与 loadChapter 共用序号，翻章后迟到的刷新结果直接作废。
    const index = readerStore.currentIndex
    const loadSeq = ++chapterLoadSeq
    readerStore.loading = true
    try {
      const chapterContent = await fetchChapterContent(index, true)
      if (chapterContent == null) return
      if (loadSeq !== chapterLoadSeq || index !== readerStore.currentIndex) return
      readerStore.setActiveChapterState(index, chapterContent, readerStore.chapterScrollProgress)
      // 正文基准刚被重抓，段评索引也要跟着刷新
      void extrasStore.loadChapterReviews(index, true)
      void preloadAroundChapter(index)
    } catch (error) {
      // 手动刷新失败不能静默：转圈停了用户会以为刷新成功
      if (loadSeq === chapterLoadSeq) {
        appStore.showToast((error as Error)?.message || '刷新章节失败', 'error')
      }
    } finally {
      if (loadSeq === chapterLoadSeq) readerStore.loading = false
    }
  }

  async function refreshChapters() {
    if (!readerStore.book) return
    readerStore.chaptersLoading = true
    try {
      readerStore.preloadedContent.clear()
      // 用户要的是刷新目录列表，不是重读当前章：此前 loadChapter(target, true)
      // 会把人踢回章首。现在只换列表，当前章正文与阅读位置不动
      const currentUrl = readerStore.chapters[readerStore.currentIndex]?.url
      if (!readerStore.book.bookId) return
      // 离线前置：手动刷新是用户盯着转圈的操作，不发必然失败的请求空等；
      // 也不拿浏览器缓存冒充「刷新成功」，如实提示离线
      if (!appStore.isOnline) {
        appStore.showToast(
          `${describeRequestFailure(new Error('离线中'), appStore.isOnline)}，无法刷新目录`,
          'warning'
        )
        return
      }
      readerStore.chapters = await getChapterList({
        bookId: readerStore.book.bookId,
        refresh: 1,
      })
      // 上游增删章导致位置漂移时，按 URL 把索引对齐回去
      if (currentUrl) {
        const newIndex = readerStore.chapters.findIndex((chapter) => chapter.url === currentUrl)
        if (newIndex >= 0 && newIndex !== readerStore.currentIndex) {
          readerStore.currentIndex = newIndex
          readerStore.book.durChapterIndex = newIndex
        }
      }
      void setBrowserCatalog(readerStore.book.bookId, readerStore.chapters)
    } catch (error) {
      // 刷新失败要如实提示：此前错误抛给点击 handler 变成 unhandled rejection，
      // 转圈停了用户却不知道失败；目录收敛到 20s 超时后这条路径会更早走到
      appStore.showToast(describeRequestFailure(error, appStore.isOnline), 'error')
    } finally {
      readerStore.chaptersLoading = false
    }
  }

  async function nextChapter() {
    if (readerStore.hasNext) {
      const completedBook = readerStore.book ? { ...readerStore.book } : null
      const completedChapter = readerStore.currentChapter ? { ...readerStore.currentChapter } : null
      await loadChapter(readerStore.currentIndex + 1)
      if (completedBook && completedChapter) {
        // 自动更新在服务端跑：fire-and-forget，关页面/换书都不受影响
        void aiBookStore.maybeAutoUpdate(completedBook, completedChapter.index)
      }
    }
  }

  async function prevChapter() {
    if (readerStore.hasPrev) {
      await loadChapter(readerStore.currentIndex - 1)
    }
  }

  function clear() {
    readerStore.book = null
    readerStore.chapters = []
    readerStore.content = ''
    readerStore.currentIndex = 0
    readerStore.chapterScrollProgress = 0
    marksStore.readChapterKeys = new Set()
    readerStore.serverProgressTime = 0
    extrasStore.resetReviews()
    readerStore.stopAutoReading()
  }

  return {
    failedChapterIndex,
    loadBook,
    restorePersistedSession,
    loadChapter,
    retryLoad,
    fetchChapterContent,
    preloadNextChapter,
    preloadAroundChapter,
    switchSource,
    invalidateChapterContentCache,
    reloadChapterAfterEdit,
    setChapterVersion,
    refreshContent,
    refreshChapters,
    nextChapter,
    prevChapter,
    clear,
  }
})
