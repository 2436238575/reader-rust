import { getChapterList, getBookContent, type ChapterContentResponse } from '../api/bookshelf'
import type { Book, BookChapter } from '../types'
import { getBrowserCachedChapter, setBrowserCachedChapter } from './browserCache'

export interface CacheProgress {
  total: number
  completed: number
  /** 截至当前抓取失败的章数；失败章跳过并继续，不打断整轮任务 */
  failed: number
  chapterTitle: string
}

/** 提前停止的原因：quota=浏览器配额写满；consecutiveErrors=上游连续失败 */
export type CacheStopReason = 'quota' | 'consecutiveErrors'

export interface BrowserCacheResult {
  total: number
  completed: number
  failed: number
  stoppedEarly?: CacheStopReason
}

/** 上游连续失败达到该值就提前收手：书源挂掉时别把整本章节都按超时烧一遍 */
const MAX_CONSECUTIVE_FAILURES = 10

/** 提前停止原因的用户可读后缀；无提前停止返回空串 */
export function cacheStopReasonText(reason?: CacheStopReason): string {
  if (reason === 'quota') return '（浏览器配额已满，提前停止）'
  if (reason === 'consecutiveErrors') return '（上游连续失败，提前停止）'
  return ''
}

export async function resolveBookChapters(book: Book) {
  if (!book.bookId) throw new Error('书籍缺少 bookId')
  return getChapterList({
    bookId: book.bookId,
  })
}

export async function cacheBookToBrowser(params: {
  book: Book
  chapters?: BookChapter[]
  startIndex?: number
  count?: number
  onProgress?: (progress: CacheProgress) => void
  signal?: { cancelled: boolean }
}): Promise<BrowserCacheResult> {
  if (!params.book.bookId) throw new Error('书籍缺少 bookId')
  const bookId = params.book.bookId
  const chapters = params.chapters || (await resolveBookChapters(params.book))
  const startIndex = Math.max(0, params.startIndex || 0)
  const sliced = chapters.slice(startIndex, params.count ? startIndex + params.count : undefined)
  let completed = 0
  let failed = 0
  let consecutiveFailures = 0
  let stoppedEarly: CacheStopReason | undefined

  for (const chapter of sliced) {
    if (params.signal?.cancelled) {
      break
    }
    // 已缓存的章带版本号去比对：一致时后端不传正文（unchanged），本地也不用
    // 重写——重复跑「缓存到浏览器」只花一次轻量请求，不再整本重传
    const cached = await getBrowserCachedChapter(bookId, chapter.url).catch(() => null)
    // 单章抓取失败（上游不可达/响应异常）计入失败并继续，不中断整轮任务；
    // 连续失败达到上限说明书源大概率挂了，提前停止省掉逐章超时
    let chapterContent: ChapterContentResponse
    try {
      chapterContent = await getBookContent({
        bookId,
        chapterUrl: chapter.url,
        knownVersionId: cached?.versionId,
      })
      if (chapterContent.unchanged && !cached) {
        // 读到 unchanged 时缓存恰好被清了（并发清理）：重拉一次全文
        chapterContent = await getBookContent({ bookId, chapterUrl: chapter.url })
      }
    } catch {
      failed += 1
      consecutiveFailures += 1
      params.onProgress?.({
        total: sliced.length,
        completed,
        failed,
        chapterTitle: chapter.title,
      })
      if (consecutiveFailures >= MAX_CONSECUTIVE_FAILURES) {
        stoppedEarly = 'consecutiveErrors'
        break
      }
      continue
    }
    consecutiveFailures = 0
    if (!chapterContent.unchanged) {
      // 浏览器配额写满时写库会抛：停下并保留已完成部分，而不是整个任务报错归零
      try {
        await setBrowserCachedChapter({
          bookId,
          chapterUrl: chapter.url,
          chapterTitle: chapter.title,
          content: chapterContent.content,
          versionId: chapterContent.versionId,
        })
      } catch {
        stoppedEarly = 'quota'
        break
      }
    }
    completed += 1
    params.onProgress?.({
      total: sliced.length,
      completed,
      failed,
      chapterTitle: chapter.title,
    })
  }
  return {
    total: sliced.length,
    completed,
    failed,
    ...(stoppedEarly ? { stoppedEarly } : {}),
  }
}
