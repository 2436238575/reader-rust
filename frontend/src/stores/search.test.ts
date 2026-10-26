// 搜索会话状态的用例：随 search store 从 bookshelf.test.ts 迁出（拆分专项收尾）。
import { beforeEach, describe, expect, it } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { useSearchStore } from './search'

describe('search state', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('starts searches in single-source scope by default', () => {
    const store = useSearchStore()

    store.startSearch('星门')

    expect(store.searchKey).toBe('星门')
    expect(store.searchScope).toBe('source')
    expect(store.searchSourceId).toBeNull()
    expect(store.searchGroup).toBe('')
  })

  it('can start a search with the active explore source selected', () => {
    const store = useSearchStore()

    store.startSearch('星门', { sourceId: 42 })

    expect(store.searchKey).toBe('星门')
    expect(store.searchScope).toBe('source')
    expect(store.searchSourceId).toBe(42)
  })

  it('clearSearch 复位全部会话字段；空关键词的 startSearch 等同清空', () => {
    const store = useSearchStore()
    store.startSearch('星门', { scope: 'group', group: '起点', sourceId: 42 })
    store.searchResults = [{ bookId: 'book_1' } as never]
    store.isSearching = true

    store.clearSearch()

    expect(store.searchKey).toBe('')
    expect(store.searchResults).toEqual([])
    expect(store.isSearching).toBe(false)
    expect(store.searchScope).toBe('source')
    expect(store.searchGroup).toBe('')
    expect(store.searchSourceId).toBeNull()

    store.startSearch('星门')
    store.startSearch('   ')
    expect(store.searchKey).toBe('')
    expect(store.searchScope).toBe('source')
  })
})
