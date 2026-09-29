import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import {
  getBookshelfWithCacheInfo,
  getBookGroups,
  deleteBook as apiDeleteBook,
  deleteBooks as apiDeleteBooks,
  saveBookGroupId as apiSaveBookGroupId,
  saveBookGroup as apiSaveBookGroup,
  deleteBookGroup as apiDeleteBookGroup,
  saveBooks as apiSaveBooks,
} from '../api/bookshelf'
import type { Book, BookGroup, SearchBook } from '../types'
import { deleteBrowserBookCache, listBrowserCacheSummary } from '../utils/browserCache'
import { isLocalBook } from '../utils/localBook'
import { useAppStore } from './app'
import {
  clearRecentReadBooks,
  getRecentReadBookKey,
  loadRecentReadBooks,
  removeRecentReadBook,
  syncRecentBooksFromBackend,
} from '../utils/recentBooks'

export const useBookshelfStore = defineStore('bookshelf', () => {
  // ─── Bookshelf ───
  const books = ref<Book[]>([])
  const recentBooks = ref<Book[]>([])
  // 最近阅读的过滤关键词，由顶栏搜索框在最近页写入
  const recentFilter = ref('')
  const loading = ref(false)

  /* 书架快照：离线（或后端不可达）时让书架还能打开。
     只存展示必需字段，避免整本书对象把 localStorage 撑爆 */
  const SHELF_SNAPSHOT_KEY = 'reader-shelf-snapshot'

  function saveShelfSnapshot() {
    try {
      const slim = books.value.map((b) => ({
        name: b.name,
        author: b.author,
        bookUrl: b.bookUrl,
        origin: b.origin,
        coverUrl: b.coverUrl,
        customCoverUrl: b.customCoverUrl,
        durChapterIndex: b.durChapterIndex,
        durChapterTitle: b.durChapterTitle,
        durChapterTime: b.durChapterTime,
        totalChapterNum: b.totalChapterNum,
        latestChapterTitle: b.latestChapterTitle,
        group: b.group,
        browserCachedChapterCount: b.browserCachedChapterCount,
      }))
      localStorage.setItem(
        SHELF_SNAPSHOT_KEY,
        JSON.stringify({ books: slim, groups: groups.value, savedAt: Date.now() })
      )
    } catch {
      // 配额满就算了——快照只是兜底
    }
  }

  function hydrateShelfSnapshot(): boolean {
    try {
      const raw = localStorage.getItem(SHELF_SNAPSHOT_KEY)
      if (!raw) return false
      const snap = JSON.parse(raw) as { books?: Book[]; groups?: BookGroup[] }
      if (!snap.books?.length) return false
      books.value = snap.books
      if (!groups.value.length && snap.groups?.length) groups.value = snap.groups
      return true
    } catch {
      return false
    }
  }
  const sorting = ref(false)

  async function refreshRecentBooks() {
    // 与后端文档做并集合并（内部有每会话一次的节流），跨浏览器同步最近阅读
    await syncRecentBooksFromBackend().catch(() => undefined)
    const browserSummaries = await listBrowserCacheSummary().catch(() => [])
    const browserMap = new Map(
      browserSummaries.map((item) => [item.bookUrl, item.cachedChapterCount])
    )
    const shelfMap = new Map(books.value.map((book) => [getRecentReadBookKey(book), book]))
    recentBooks.value = loadRecentReadBooks().map((entry) => {
      const shelfBook = shelfMap.get(getRecentReadBookKey(entry))
      const merged = shelfBook
        ? {
            ...entry,
            ...shelfBook,
            recentReadAt: entry.recentReadAt,
            durChapterTime: entry.recentReadAt,
          }
        : entry
      return {
        ...merged,
        browserCachedChapterCount: isLocalBook(merged)
          ? 0
          : browserMap.get(merged.bookUrl) || merged.browserCachedChapterCount || 0,
      }
    })
  }

  async function removeRecentBook(book: Pick<Book, 'bookUrl' | 'origin'>) {
    removeRecentReadBook(book)
    await refreshRecentBooks()
  }

  async function clearAllRecentBooks() {
    clearRecentReadBooks()
    await refreshRecentBooks()
  }

  async function fetchBooks() {
    loading.value = true
    try {
      const [serverBooks, browserSummaries] = await Promise.all([
        getBookshelfWithCacheInfo(),
        listBrowserCacheSummary().catch(() => []),
      ])
      const browserMap = new Map(
        browserSummaries.map((item) => [item.bookUrl, item.cachedChapterCount])
      )
      books.value = serverBooks.map((book) => ({
        ...book,
        browserCachedChapterCount: isLocalBook(book) ? 0 : browserMap.get(book.bookUrl) || 0,
      }))
      await refreshRecentBooks()
      saveShelfSnapshot()
    } catch (error) {
      // 离线/后端不可达：书架回退到最近一次成功的快照，不是空书架
      if (!books.value.length && hydrateShelfSnapshot()) {
        useAppStore().showToast('离线中：书架展示的是最近缓存', 'warning')
        return
      }
      throw error
    } finally {
      loading.value = false
    }
  }
  async function removeBook(book: Book) {
    await apiDeleteBook(book)
    await deleteBrowserBookCache(book.bookUrl).catch(() => undefined)
    books.value = books.value.filter((b) => b.bookUrl !== book.bookUrl)
    await refreshRecentBooks()
  }

  // ─── Groups ───
  const groups = ref<BookGroup[]>([])
  const activeGroupId = ref<number>(-1) // -1 = all

  const displayGroups = computed(() => {
    const all: BookGroup = { groupId: -1, groupName: '全部' }
    const ungrouped: BookGroup = { groupId: 0, groupName: '未分组' }
    return [all, ...groups.value, ungrouped]
  })

  // 没有自定义分组且所有书都未分组（含空书架）时，「全部/未分组」两个 Tab
  // 完全等价，整个分组 Tab 行隐藏
  const showGroupTabs = computed(
    () => groups.value.length > 0 || books.value.some((b) => b.group && b.group !== 0)
  )

  const filteredBooks = computed(() => {
    if (activeGroupId.value === -1) return books.value
    if (activeGroupId.value === 0) {
      return books.value.filter((b) => !b.group || b.group === 0)
    }
    return books.value.filter((b) => b.group && (b.group & activeGroupId.value) !== 0)
  })

  async function fetchGroups() {
    try {
      groups.value = await getBookGroups()
    } catch {
      groups.value = []
    }
  }

  async function saveGroup(groupName: string, groupId = 0) {
    await apiSaveBookGroup({
      groupId,
      groupName,
      orderNo: groups.value.length,
    })
    await fetchGroups()
    return groups.value.find((group) => group.groupName === groupName)?.groupId || groupId
  }

  async function removeGroup(groupId: number) {
    await apiDeleteBookGroup(groupId)
    groups.value = groups.value.filter((group) => group.groupId !== groupId)
    books.value = books.value.map((book) => {
      if (book.group && (book.group & groupId) !== 0) {
        return { ...book, group: book.group & ~groupId }
      }
      return book
    })
  }

  // ─── Search ───
  const searchResults = ref<SearchBook[]>([])
  const isSearching = ref(false)
  const searchKey = ref('')
  const searchScope = ref<'all' | 'group' | 'source'>('source')
  const searchGroup = ref('')
  const searchSourceUrl = ref('')

  function startSearch(
    key: string,
    options: {
      scope?: 'all' | 'group' | 'source'
      group?: string
      sourceUrl?: string
    } = {}
  ) {
    const nextKey = key.trim()
    if (!nextKey) {
      clearSearch()
      return
    }

    searchScope.value = options.scope || 'source'
    searchGroup.value = options.group || ''
    searchSourceUrl.value = options.sourceUrl || ''
    searchKey.value = nextKey
  }

  function clearSearch() {
    searchResults.value = []
    searchKey.value = ''
    isSearching.value = false
    searchScope.value = 'source'
    searchGroup.value = ''
    searchSourceUrl.value = ''
  }

  // ─── Edit mode and Selection ───
  const editMode = ref(false)
  const selectedBookUrls = ref<Set<string>>(new Set())

  function toggleSelection(url: string) {
    if (selectedBookUrls.value.has(url)) {
      selectedBookUrls.value.delete(url)
    } else {
      selectedBookUrls.value.add(url)
    }
  }

  function selectAll() {
    filteredBooks.value.forEach((b) => selectedBookUrls.value.add(b.bookUrl))
  }

  function clearSelection() {
    selectedBookUrls.value.clear()
  }

  async function bulkDelete() {
    const toDelete = books.value
      .filter((b) => selectedBookUrls.value.has(b.bookUrl))
      .map((b) => ({ bookUrl: b.bookUrl, origin: b.origin }))

    if (toDelete.length === 0) return
    await apiDeleteBooks(toDelete as Book[])
    await Promise.all(
      toDelete.map((book) => deleteBrowserBookCache(book.bookUrl).catch(() => undefined))
    )
    books.value = books.value.filter((b) => !selectedBookUrls.value.has(b.bookUrl))
    clearSelection()
  }

  async function bulkSetGroup(groupId: number) {
    const urls = Array.from(selectedBookUrls.value)
    for (const url of urls) {
      await apiSaveBookGroupId(url, groupId)
    }
    // Refresh to get updated groups
    await fetchBooks()
    clearSelection()
  }

  async function reorderBooks(draggedUrl: string, targetUrl: string) {
    if (!draggedUrl || !targetUrl || draggedUrl === targetUrl) return

    const snapshot = books.value.slice()
    const fromIndex = snapshot.findIndex((book) => book.bookUrl === draggedUrl)
    const toIndex = snapshot.findIndex((book) => book.bookUrl === targetUrl)
    if (fromIndex === -1 || toIndex === -1 || fromIndex === toIndex) return

    const next = snapshot.slice()
    const [moved] = next.splice(fromIndex, 1)
    next.splice(toIndex, 0, moved)

    books.value = next
    sorting.value = true
    try {
      await apiSaveBooks(next)
    } catch (error) {
      books.value = snapshot
      throw error
    } finally {
      sorting.value = false
    }
  }

  async function moveBookToFront(bookUrl: string) {
    if (!bookUrl || books.value.length <= 1) return

    const snapshot = books.value.slice()
    const fromIndex = snapshot.findIndex((book) => book.bookUrl === bookUrl)
    if (fromIndex <= 0) return

    const next = snapshot.slice()
    const [moved] = next.splice(fromIndex, 1)
    next.unshift(moved)

    books.value = next
    sorting.value = true
    try {
      await apiSaveBooks(next)
    } catch (error) {
      books.value = snapshot
      throw error
    } finally {
      sorting.value = false
    }
  }

  return {
    books,
    recentBooks,
    recentFilter,
    loading,
    sorting,
    fetchBooks,
    removeBook,
    refreshRecentBooks,
    removeRecentBook,
    clearAllRecentBooks,
    groups,
    activeGroupId,
    displayGroups,
    filteredBooks,
    showGroupTabs,
    fetchGroups,
    saveGroup,
    removeGroup,
    searchResults,
    isSearching,
    searchKey,
    searchScope,
    searchGroup,
    searchSourceUrl,
    startSearch,
    clearSearch,
    editMode,
    selectedBookUrls,
    toggleSelection,
    selectAll,
    clearSelection,
    bulkDelete,
    bulkSetGroup,
    reorderBooks,
    moveBookToFront,
  }
})
