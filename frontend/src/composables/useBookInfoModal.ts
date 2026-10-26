import { ref } from 'vue'
import type { Router } from 'vue-router'
import type { useReaderStore } from '../stores/reader'
import type { useReaderContentStore } from '../stores/readerContent'
import type { useAppStore } from '../stores/app'
import type { Book, SearchBook } from '../types'
import { getBookInfo } from '../api/bookshelf'

type ReaderStore = ReturnType<typeof useReaderStore>
type ReaderContentStore = ReturnType<typeof useReaderContentStore>
type AppStore = ReturnType<typeof useAppStore>

/// 书籍详情弹窗粘合：打开时先铺当前会话的书（含实时进度），再异步拉最新详情；
/// 弹窗内换源成功后要重开阅读会话，否则弹窗一关阅读区还握着旧源。
export function useBookInfoModal(
  store: ReaderStore,
  contentStore: ReaderContentStore,
  appStore: AppStore,
  router: Router
) {
  const showBookInfo = ref(false)
  const bookInfoBook = ref<Book | null>(null)

  async function openInfo() {
    if (!store.book) return
    showBookInfo.value = true
    bookInfoBook.value = {
      ...store.book,
      durChapterIndex: store.currentIndex,
      durChapterTitle: store.currentChapter?.title || store.book.durChapterTitle,
    }
    try {
      if (!store.book.bookId) return
      const latest = await getBookInfo(store.book.bookId)
      bookInfoBook.value = {
        ...store.book,
        ...latest,
        durChapterIndex: store.currentIndex,
        durChapterTitle:
          store.currentChapter?.title || latest.durChapterTitle || store.book.durChapterTitle,
      }
    } catch {
      appStore.showToast('获取书籍详情失败，已显示当前缓存信息', 'warning')
    }
  }

  // 详情弹窗内换源成功：后台书籍已落库，但当前阅读会话还握着旧源的 book 对象，
  // 不刷新的话弹窗一关，阅读区仍是旧源的报错。用换源结果重开一次会话（含目录）。
  async function handleBookSwitched(updated: Book | SearchBook) {
    const b = updated as Book
    bookInfoBook.value = b
    await contentStore.loadBook(b).catch(() => undefined)
    await contentStore.loadChapter(b.durChapterIndex ?? 0)
  }

  function openAiBook() {
    if (!store.book) return
    router.push({
      name: 'ai-book',
      query: { bookId: store.book.bookId },
    })
  }

  return { showBookInfo, bookInfoBook, openInfo, handleBookSwitched, openAiBook }
}
