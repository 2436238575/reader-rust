import { getChapterList, getBookContent } from '../api/bookshelf'
import type { Book, BookChapter } from '../types'
import { getBrowserCachedChapter, setBrowserCachedChapter } from './browserCache'

export interface CacheProgress {
  total: number
  completed: number
  chapterTitle: string
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
}) {
  if (!params.book.bookId) throw new Error('书籍缺少 bookId')
  const bookId = params.book.bookId
  const chapters = params.chapters || (await resolveBookChapters(params.book))
  const startIndex = Math.max(0, params.startIndex || 0)
  const sliced = chapters.slice(startIndex, params.count ? startIndex + params.count : undefined)
  let completed = 0

  for (const chapter of sliced) {
    if (params.signal?.cancelled) {
      break
    }
    // 已缓存的章带版本号去比对：一致时后端不传正文（unchanged），本地也不用
    // 重写——重复跑「缓存到浏览器」只花一次轻量请求，不再整本重传
    const cached = await getBrowserCachedChapter(bookId, chapter.url).catch(() => null)
    let chapterContent = await getBookContent({
      bookId,
      chapterUrl: chapter.url,
      knownVersionId: cached?.versionId,
    })
    if (chapterContent.unchanged && !cached) {
      // 读到 unchanged 时缓存恰好被清了（并发清理）：重拉一次全文
      chapterContent = await getBookContent({ bookId, chapterUrl: chapter.url })
    }
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
        break
      }
    }
    completed += 1
    params.onProgress?.({
      total: sliced.length,
      completed,
      chapterTitle: chapter.title,
    })
  }
  return {
    total: sliced.length,
    completed,
  }
}
