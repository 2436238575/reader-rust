import { getChapterList, getBookContent } from '../api/bookshelf'
import { listChapterEdits } from '../api/chapterEdit'
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
  // 编辑章跳过浏览器缓存：编辑版只存在于服务端，本地存下的上游正文会在
  // 离线兜底时顶掉另一台设备的编辑内容。清单拉取失败当作没有编辑章处理，
  // 不能因为拿不到清单就中断整批缓存（阅读路径仍会按服务端版本正确显示）
  let editedUrls = new Set<string>()
  try {
    editedUrls = new Set(await listChapterEdits(bookId))
  } catch {
    /* 离线/失败时按空集合继续 */
  }
  const startIndex = Math.max(0, params.startIndex || 0)
  const sliced = chapters.slice(startIndex, params.count ? startIndex + params.count : undefined)
  let completed = 0

  for (const chapter of sliced) {
    if (params.signal?.cancelled) {
      break
    }
    // 编辑章不落 IndexedDB，但进度照常计完成：这些章对用户而言同样是
    // 「处理过了」，否则缓存整本书时进度条永远到不了 100%
    if (editedUrls.has(chapter.url)) {
      completed += 1
      params.onProgress?.({
        total: sliced.length,
        completed,
        chapterTitle: chapter.title,
      })
      continue
    }
    const chapterContent = await getBookContent({
      bookId,
      chapterUrl: chapter.url,
    })
    // 浏览器配额写满时写库会抛：停下并保留已完成部分，而不是整个任务报错归零
    try {
      await setBrowserCachedChapter({
        bookId,
        chapterUrl: chapter.url,
        chapterTitle: chapter.title,
        content: chapterContent.content,
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
