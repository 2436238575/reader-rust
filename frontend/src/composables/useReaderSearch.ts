import { computed, nextTick, ref } from 'vue'
import type { useReaderStore } from '../stores/reader'

type ReaderStore = ReturnType<typeof useReaderStore>

export interface SearchResultItem {
  chapterIndex: number
  chapterTitle: string
  snippet: string
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

  function runSearch() {
    if (!searchQuery.value.trim()) {
      searchResults.value = []
      bookSearchStatus.value = ''
      return
    }
    runChapterSearch()
  }

  function scrollToMatch(allowRetry = true) {
    nextTick(() => {
      const matches = document.querySelectorAll('.search-highlight')
      if (!matches.length) {
        // 横翻模式的分页是异步重建的，跨章跳转后高亮可能还没落进页里
        if (allowRetry) window.setTimeout(() => scrollToMatch(false), 150)
        return
      }
      let targetIndex = searchIndex.value
      if (pendingSearchResult.value) {
        const snippet = pendingSearchResult.value.snippet.replace(/\.\.\./g, '').trim()
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
    if (showSearch.value && searchQuery.value) {
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
    toggleSearch,
    openSearch,
    closeSearch,
    runSearch,
    nextSearchResult,
    prevSearchResult,
    jumpToSearchResult,
    handleContentUpdated,
    handlePresentationUpdated,
  }
}
