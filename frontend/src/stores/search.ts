// 搜索会话状态：从 bookshelf store 拆出的独立 store（reader store 拆分专项收尾）。
// 与书架实体零耦合：SearchResults 直接读写结果集与范围筛选，SearchView 以
// URL（#/search?q=...）驱动 startSearch/clearSearch；书架只保留书籍与分组数据。

import { defineStore } from 'pinia'
import { ref } from 'vue'
import type { SearchBookDisplay } from '../types'

export const useSearchStore = defineStore('search', () => {
  const searchResults = ref<SearchBookDisplay[]>([])
  const isSearching = ref(false)
  const searchKey = ref('')
  const searchScope = ref<'all' | 'group' | 'source'>('source')
  const searchGroup = ref('')
  const searchSourceId = ref<number | null>(null)

  function startSearch(
    key: string,
    options: {
      scope?: 'all' | 'group' | 'source'
      group?: string
      sourceId?: number | null
    } = {}
  ) {
    const nextKey = key.trim()
    if (!nextKey) {
      clearSearch()
      return
    }

    searchScope.value = options.scope || 'source'
    searchGroup.value = options.group || ''
    searchSourceId.value = options.sourceId ?? null
    searchKey.value = nextKey
  }

  function clearSearch() {
    searchResults.value = []
    searchKey.value = ''
    isSearching.value = false
    searchScope.value = 'source'
    searchGroup.value = ''
    searchSourceId.value = null
  }

  return {
    searchResults,
    isSearching,
    searchKey,
    searchScope,
    searchGroup,
    searchSourceId,
    startSearch,
    clearSearch,
  }
})
