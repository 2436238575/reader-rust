import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { useBookshelfStore } from '../stores/bookshelf'
import { useReaderStore } from '../stores/reader'
import type { Book, SearchBook } from '../types'

/**
 * 「打开书并跳到阅读页」的唯一入口：置顶书架（书不在书架时本地 no-op、不发请求）
 * → 后台并行开始加载书籍 → 跳转路由 → 等加载完成后定位章节。
 * 书架/最近/发现/搜索/详情弹窗统一走这里，避免各写一份后行为漂移。
 */
export function useOpenBook() {
  const router = useRouter()
  const shelfStore = useBookshelfStore()
  const readerStore = useReaderStore()
  // 防连点重入：同一本书的打开流程进行中忽略重复触发
  const openingBookUrl = ref('')

  async function openBook(book: Book | SearchBook, chapterIndex?: number) {
    const target = book as Book
    if (!target.origin || !target.bookUrl) return
    if (openingBookUrl.value === target.bookUrl) return

    openingBookUrl.value = target.bookUrl
    try {
      await shelfStore.moveBookToFront(target.bookUrl).catch(() => undefined)
      const loadBookTask = readerStore.loadBook(target)
      await router.push('/reader')
      await loadBookTask
      await readerStore.loadChapter(chapterIndex ?? target.durChapterIndex ?? 0)
    } finally {
      openingBookUrl.value = ''
    }
  }

  return { openBook }
}
