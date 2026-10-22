import { computed, nextTick, ref } from 'vue'
import type { useReaderStore } from '../stores/reader'
import { searchCachedContent } from '../api/search'

type ReaderStore = ReturnType<typeof useReaderStore>

export interface SearchResultItem {
  chapterIndex: number
  chapterTitle: string
  snippet: string
}

export type ReaderSearchScope = 'chapter' | 'book'

export interface BookSearchCoverage {
  cachedChapters: number
  totalChapters: number
  truncated: boolean
}

interface ReaderSearchOptions {
  /**
   * 命中项定位钩子。横翻分页模式容器 overflow:hidden + transform 翻页，
   * scrollIntoView 不生效，由调用方按页跳转；不传则维持 scrollIntoView。
   */
  locateMatch?: (el: HTMLElement) => void
}

export function useReaderSearch(store: ReaderStore, options: ReaderSearchOptions = {}) {
  const showSearch = ref(false)
  const searchQuery = ref('')
  const searchResults = ref<SearchResultItem[]>([])
  const searchIndex = ref(0)
  const bookSearchStatus = ref('')
  const pendingSearchResult = ref<SearchResultItem | null>(null)
  const searchCount = computed(() => searchResults.value.length)
  /** 搜索范围：当前章（纯客户端正则）/ 全书（后端缓存内检索） */
  const searchScope = ref<ReaderSearchScope>('chapter')
  const bookSearchLoading = ref(false)
  const bookSearchCoverage = ref<BookSearchCoverage | null>(null)

  // 输入框的 focus 由 ReaderSearchPanel 自己负责
  function toggleSearch() {
    showSearch.value = !showSearch.value
    if (!showSearch.value) {
      closeSearch()
    }
  }

  function openSearch() {
    showSearch.value = true
  }

  function closeSearch() {
    showSearch.value = false
    searchQuery.value = ''
    searchIndex.value = 0
    searchResults.value = []
    bookSearchStatus.value = ''
    pendingSearchResult.value = null
    searchScope.value = 'chapter'
    bookSearchLoading.value = false
    bookSearchCoverage.value = null
    clearScrollRetryTimer()
  }

  function buildSearchSnippet(text: string, keyword: string, startIndex: number) {
    const radius = 18
    const start = Math.max(0, startIndex - radius)
    const end = Math.min(text.length, startIndex + keyword.length + radius)
    return `${start > 0 ? '...' : ''}${text.slice(start, end).trim()}${end < text.length ? '...' : ''}`
  }

  function extractSearchResults(
    text: string,
    chapterIndex: number,
    chapterTitle: string,
    keyword: string,
    limit = Infinity
  ) {
    if (!keyword) return [] as SearchResultItem[]
    const normalizedText = text.replace(/\s+/g, ' ').trim()
    if (!normalizedText) return [] as SearchResultItem[]
    const escaped = keyword.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    const regex = new RegExp(escaped, 'gi')
    const list: SearchResultItem[] = []
    let match: RegExpExecArray | null
    while ((match = regex.exec(normalizedText)) && list.length < limit) {
      list.push({
        chapterIndex,
        chapterTitle,
        snippet: buildSearchSnippet(normalizedText, match[0], match.index),
      })
    }
    return list
  }

  function runChapterSearch() {
    searchIndex.value = 0
    if (!searchQuery.value) {
      searchResults.value = []
      bookSearchStatus.value = ''
      return
    }
    const plainText = (store.displayContent || '').replace(/<[^>]+>/g, '')
    searchResults.value = extractSearchResults(
      plainText,
      store.currentIndex,
      store.currentChapter?.title || '当前章节',
      searchQuery.value
    )
    nextTick(() => {
      if (searchResults.value.length) scrollToMatch()
    })
    bookSearchStatus.value = searchResults.value.length
      ? `共 ${searchResults.value.length} 条结果`
      : '当前章节没有匹配内容'
  }

  /** 全书搜索：后端在已缓存章节正文里检索（不触发任何上游请求）。 */
  async function runBookSearch() {
    searchIndex.value = 0
    const query = searchQuery.value.trim()
    const bookId = store.book?.bookId
    if (!query || !bookId) {
      searchResults.value = []
      bookSearchCoverage.value = null
      bookSearchStatus.value = ''
      return
    }
    bookSearchLoading.value = true
    bookSearchCoverage.value = null
    try {
      const outcome = await searchCachedContent(bookId, query)
      // 后端按章聚合 snippets，这里展平成与章内搜索同形态的逐条结果
      // （跳转定位靠 snippet 前缀匹配，同章多条各自可点）
      searchResults.value = outcome.results.flatMap((hit) =>
        hit.snippets.map((snippet) => ({
          chapterIndex: hit.chapterIndex,
          chapterTitle: hit.chapterTitle,
          snippet,
        }))
      )
      bookSearchCoverage.value = {
        cachedChapters: outcome.cachedChapters,
        totalChapters: outcome.totalChapters,
        truncated: outcome.truncated,
      }
      const base = searchResults.value.length
        ? `共 ${searchResults.value.length} 条结果`
        : '没有匹配内容'
      const coverageText = outcome.totalChapters
        ? `（已检索 ${outcome.cachedChapters}/${outcome.totalChapters} 个已缓存章节${outcome.truncated ? '，结果过多已截断' : ''}）`
        : '（本书还没有已缓存的章节可供检索）'
      bookSearchStatus.value = base + coverageText
      if (searchResults.value.length) {
        const first = searchResults.value[0]
        await jumpToSearchResult(first, 0)
      }
    } catch (error) {
      searchResults.value = []
      bookSearchStatus.value = (error as Error)?.message || '全书搜索失败'
    } finally {
      bookSearchLoading.value = false
    }
  }

  function runSearch() {
    if (!searchQuery.value.trim()) {
      searchResults.value = []
      bookSearchStatus.value = ''
      return
    }
    if (searchScope.value === 'book') {
      void runBookSearch()
      return
    }
    runChapterSearch()
  }

  /** 切换范围：已加载的结果集与另一套不兼容，清掉重来 */
  function setSearchScope(scope: ReaderSearchScope) {
    if (searchScope.value === scope) return
    searchScope.value = scope
    searchResults.value = []
    searchIndex.value = 0
    bookSearchCoverage.value = null
    bookSearchStatus.value = ''
  }

  /** 横翻分页异步重建时的 150ms 重试定时器：关面板/卸载时清掉 */
  let scrollRetryTimer: number | null = null

  function clearScrollRetryTimer() {
    if (scrollRetryTimer) {
      clearTimeout(scrollRetryTimer)
      scrollRetryTimer = null
    }
  }

  function scrollToMatch(allowRetry = true) {
    nextTick(() => {
      const matches = document.querySelectorAll('.search-highlight')
      if (!matches.length) {
        // 横翻模式的分页是异步重建的，跨章跳转后高亮可能还没落进页里
        if (allowRetry) {
          clearScrollRetryTimer()
          scrollRetryTimer = window.setTimeout(() => {
            scrollRetryTimer = null
            scrollToMatch(false)
          }, 150)
        }
        return
      }
      let targetIndex = searchIndex.value
      if (pendingSearchResult.value) {
        // 章内搜索摘要用英文三点、后端全书摘要用中文省略号，都剥掉再取前缀匹配
        const snippet = pendingSearchResult.value.snippet.replace(/\.\.\.|…/g, '').trim()
        const foundIndex = Array.from(matches).findIndex((match) => {
          const line = (match.parentElement?.innerText || '').replace(/\s+/g, ' ').trim()
          return !!snippet && line.includes(snippet.slice(0, Math.min(12, snippet.length)))
        })
        if (foundIndex >= 0) targetIndex = foundIndex
        pendingSearchResult.value = null
      }
      const target = matches[targetIndex] as HTMLElement | undefined
      if (!target) return
      if (options.locateMatch) options.locateMatch(target)
      else target.scrollIntoView({ block: 'center', behavior: 'smooth' })
      matches.forEach((item) => item.classList.remove('current-match'))
      target.classList.add('current-match')
    })
  }

  function nextSearchResult() {
    if (!searchCount.value) return
    searchIndex.value = (searchIndex.value + 1) % searchCount.value
    scrollToMatch()
  }

  function prevSearchResult() {
    if (!searchCount.value) return
    searchIndex.value = (searchIndex.value - 1 + searchCount.value) % searchCount.value
    scrollToMatch()
  }

  async function jumpToSearchResult(result: SearchResultItem, idx: number) {
    searchIndex.value = idx
    pendingSearchResult.value = result
    if (result.chapterIndex !== store.currentIndex) {
      await store.loadChapter(result.chapterIndex)
    }
    nextTick(() => {
      scrollToMatch()
    })
  }

  function handleContentUpdated() {
    // 内容更新只重跑当前章的纯客户端匹配；全书结果集来自后端缓存检索，不随翻章失效
    if (showSearch.value && searchQuery.value && searchScope.value === 'chapter') {
      runChapterSearch()
    }
  }

  // 版式/字号切换与内容更新对搜索的重跑要求相同
  const handlePresentationUpdated = handleContentUpdated

  return {
    showSearch,
    searchQuery,
    searchResults,
    searchIndex,
    searchCount,
    bookSearchStatus,
    searchScope,
    bookSearchLoading,
    bookSearchCoverage,
    toggleSearch,
    openSearch,
    closeSearch,
    runSearch,
    setSearchScope,
    nextSearchResult,
    prevSearchResult,
    jumpToSearchResult,
    handleContentUpdated,
    handlePresentationUpdated,
  }
}
