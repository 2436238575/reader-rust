import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import {
  getBookshelfWithCacheInfo,
  getBookGroups,
  addToShelf,
  removeFromShelf,
  batchRemoveFromShelf,
  getShelfBook,
  saveBookGroupId as apiSaveBookGroupId,
  saveBookGroup as apiSaveBookGroup,
  deleteBookGroup as apiDeleteBookGroup,
  saveShelfOrder as apiSaveShelfOrder,
} from '../api/bookshelf'
import type { Book, BookGroup, ShelfBook } from '../types'
import { deleteBrowserBookCache, listBrowserCacheSummary } from '../utils/browserCache'
import { isLocalBook } from '../utils/localBook'
import { describeRequestFailure } from '../utils/requestFailure'
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
  const books = ref<ShelfBook[]>([])
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
        bookId: b.bookId,
        bookSourceId: b.bookSourceId,
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
      browserSummaries.map((item) => [item.bookId, item.cachedChapterCount])
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
          : browserMap.get(merged.bookId || '') || merged.browserCachedChapterCount || 0,
      }
    })
  }

  async function removeRecentBook(book: Pick<Book, 'bookId'>) {
    removeRecentReadBook(book)
    await refreshRecentBooks()
  }

  async function clearAllRecentBooks() {
    clearRecentReadBooks()
    await refreshRecentBooks()
  }

  /* 书架脏标记：会改书架展示数据（阅读进度、本地/远程缓存章数等）的地方
     同步标脏，返回书架（onActivated）时只在脏时重拉，零改动不再全量请求。
     dirty 初始 true 保证首载必拉；generation 处理「fetch 进行中又标脏」的竞态——
     成功后仅当代数没变才清 dirty；失败（含快照兜底路径）保持脏以便下次重试 */
  const shelfDirty = ref(true)
  let shelfGeneration = 0
  let inFlightFetch: Promise<void> | null = null

  function markShelfDirty() {
    shelfDirty.value = true
    shelfGeneration += 1
  }

  async function fetchBooks() {
    // 同一时刻只发一轮请求：并发调用共享同一个 Promise（快速切换页面等场景不双拉）
    if (inFlightFetch) return inFlightFetch
    const generationAtStart = shelfGeneration
    loading.value = true
    inFlightFetch = (async () => {
      try {
        const [serverBooks, browserSummaries] = await Promise.all([
          getBookshelfWithCacheInfo(),
          listBrowserCacheSummary().catch(() => []),
        ])
        const browserMap = new Map(
          browserSummaries.map((item) => [item.bookId, item.cachedChapterCount])
        )
        books.value = serverBooks.map((book) => ({
          ...book,
          browserCachedChapterCount: isLocalBook(book) ? 0 : browserMap.get(book.bookId || '') || 0,
        }))
        await refreshRecentBooks()
        saveShelfSnapshot()
        if (shelfGeneration === generationAtStart) {
          shelfDirty.value = false
        }
      } catch (error) {
        // 失败一定要保持脏：调用方可能在书架已干净时主动重拉（加书/换源后），
        // 这次失败若把 dirty 留在 false，返回书架时 onActivated 会直接跳过，
        // 新增/变更的内容要等到下一次其他标脏操作才出现
        shelfDirty.value = true
        // 失败回退到最近一次成功的快照，不是空书架。原因要如实——
        // 后端可达但上游挂了（502）与浏览器离线是两回事
        if (!books.value.length && hydrateShelfSnapshot()) {
          const appStore = useAppStore()
          appStore.showToast(
            `${describeRequestFailure(error, appStore.isOnline)}：书架展示的是最近缓存`,
            'warning'
          )
          return
        }
        throw error
      } finally {
        loading.value = false
        inFlightFetch = null
      }
    })()
    return inFlightFetch
  }
  /* 单本入架：走 v1 PUT（201 新建 / 200 已在架）。已在架时服务端不改进度与
     元数据，本地条目原样保留；新建时按传入的展示字段就地追加，不整架重拉
     （千级书架加一本就全量 GET 一遍太浪费）。失败原样抛给调用方 toast */
  async function addBook(book: Partial<Book>): Promise<Book> {
    const bookId = book.bookId
    if (!bookId) throw new Error('书籍缺少 bookId，无法入架')
    // 展示字段随请求体带给服务端：跳过回源详情，发现页直接入架不再等
    const { created } = await addToShelf(bookId, {
      name: book.name,
      author: book.author,
      coverUrl: book.coverUrl,
      intro: book.intro,
      kind: book.kind,
    })
    const index = books.value.findIndex((b) => b.bookId === bookId)
    const existing = index >= 0 ? books.value[index] : undefined
    // 已在架：PUT 是幂等的，服务端不重置进度/元数据，本地条目保持原样
    if (!created && existing) return existing

    // 新入架：201 后用服务端条目对齐一次——封面统一成图片管道 id（搜索结果
    // 里可能是会过期的签名直出地址），章节数等也以后端落库为准；拉不到
    // （离线等）再用调用方传入的字段就地立起来
    const saved = created ? await getShelfBook(bookId).catch(() => null) : null
    const next: ShelfBook = {
      ...(saved ?? book),
      bookId,
      name: saved?.name ?? book.name ?? existing?.name ?? '',
      author: saved?.author ?? book.author ?? existing?.author ?? '',
      bookUrl: saved?.bookUrl ?? book.bookUrl ?? existing?.bookUrl ?? '',
      origin: saved?.origin ?? book.origin ?? existing?.origin ?? '',
      durChapterIndex:
        saved?.durChapterIndex ?? book.durChapterIndex ?? existing?.durChapterIndex ?? 0,
      durChapterPos:
        saved?.durChapterPos ?? book.durChapterPos ?? existing?.durChapterPos ?? 0,
      // 书架列表注入的两个缓存章数字段：本地来源新书为 0，远程沿用旧值
      browserCachedChapterCount: existing?.browserCachedChapterCount ?? 0,
      cachedChapterCount: saved?.cachedChapterCount ?? existing?.cachedChapterCount,
    }
    if (index >= 0) {
      books.value[index] = next
    } else {
      books.value.push(next)
    }
    saveShelfSnapshot()
    // 最近阅读里若躺着这本书（读过又移出书架等），归并最新书架数据
    if (recentBooks.value.some((b) => b.bookId === bookId)) {
      await refreshRecentBooks()
    }
    return next
  }

  async function removeBook(book: Book) {
    if (!book.bookId) return
    await removeFromShelf(book.bookId)
    await deleteBrowserBookCache(book.bookId).catch(() => undefined)
    books.value = books.value.filter((b) => b.bookId !== book.bookId)
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
  // 搜索会话状态已拆到 stores/search（拆分专项收尾）；书架只管书籍与分组。

  // ─── Edit mode and Selection ───
  const editMode = ref(false)
  const selectedBookIds = ref<Set<string>>(new Set())

  function toggleSelection(bookId: string) {
    if (selectedBookIds.value.has(bookId)) {
      selectedBookIds.value.delete(bookId)
    } else {
      selectedBookIds.value.add(bookId)
    }
  }

  function selectAll() {
    filteredBooks.value.forEach((b) => b.bookId && selectedBookIds.value.add(b.bookId))
  }

  function clearSelection() {
    selectedBookIds.value.clear()
  }

  async function bulkDelete() {
    const toDelete = books.value
      .filter((b) => b.bookId && selectedBookIds.value.has(b.bookId))
      .map((b) => b.bookId!)

    if (toDelete.length === 0) return
    await batchRemoveFromShelf(toDelete)
    await Promise.all(
      toDelete.map((bookId) => deleteBrowserBookCache(bookId).catch(() => undefined))
    )
    books.value = books.value.filter((b) => !b.bookId || !selectedBookIds.value.has(b.bookId))
    clearSelection()
  }

  async function bulkSetGroup(groupId: number) {
    const ids = Array.from(selectedBookIds.value)
    for (const id of ids) {
      await apiSaveBookGroupId(id, groupId)
    }
    // Refresh to get updated groups
    await fetchBooks()
    clearSelection()
  }

  async function reorderBooks(draggedId: string, targetId: string) {
    if (!draggedId || !targetId || draggedId === targetId) return

    const snapshot = books.value.slice()
    const fromIndex = snapshot.findIndex((book) => book.bookId === draggedId)
    const toIndex = snapshot.findIndex((book) => book.bookId === targetId)
    if (fromIndex === -1 || toIndex === -1 || fromIndex === toIndex) return

    const next = snapshot.slice()
    const [moved] = next.splice(fromIndex, 1)
    next.splice(toIndex, 0, moved)

    const ids = next.map((book) => book.bookId).filter((id): id is string => Boolean(id))
    if (ids.length !== next.length) {
      // 本地条目缺 bookId：不发送注定被服务端拒绝的不完整集合
      await fetchBooks().catch(() => undefined)
      return
    }
    books.value = next
    sorting.value = true
    try {
      await apiSaveShelfOrder(ids)
    } catch (error) {
      books.value = snapshot
      // 视图过期（另一标签页加删书）时服务端会持续 400；拉一次最新书架自愈
      await fetchBooks().catch(() => undefined)
      throw error
    } finally {
      sorting.value = false
    }
  }

  async function moveBookToFront(bookId: string) {
    if (!bookId || books.value.length <= 1) return

    const snapshot = books.value.slice()
    const fromIndex = snapshot.findIndex((book) => book.bookId === bookId)
    if (fromIndex <= 0) return

    const next = snapshot.slice()
    const [moved] = next.splice(fromIndex, 1)
    next.unshift(moved)

    const ids = next.map((book) => book.bookId).filter((id): id is string => Boolean(id))
    if (ids.length !== next.length) {
      await fetchBooks().catch(() => undefined)
      return
    }
    books.value = next
    sorting.value = true
    try {
      await apiSaveShelfOrder(ids)
    } catch (error) {
      books.value = snapshot
      await fetchBooks().catch(() => undefined)
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
    shelfDirty,
    markShelfDirty,
    fetchBooks,
    addBook,
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
    editMode,
    selectedBookIds,
    toggleSelection,
    selectAll,
    clearSelection,
    bulkDelete,
    bulkSetGroup,
    reorderBooks,
    moveBookToFront,
  }
})
