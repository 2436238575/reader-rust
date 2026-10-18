import { getChapterList, getBookContent } from '../api/bookshelf'
import type { Book, BookChapter } from '../types'
import { setBrowserCachedChapter } from './browserCache'

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
    const content = await getBookContent({
      bookId,
      chapterUrl: chapter.url,
    })
    // 浏览器配额写满时写库会抛：停下并保留已完成部分，而不是整个任务报错归零
    try {
      await setBrowserCachedChapter({
        bookId,
        chapterUrl: chapter.url,
        chapterTitle: chapter.title,
        content,
      })
    } catch {
      break
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
