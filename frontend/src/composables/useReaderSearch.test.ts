// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useReaderSearch } from './useReaderSearch'
import { searchCachedContent } from '../api/search'

vi.mock('../api/search', () => ({
  searchCachedContent: vi.fn(),
}))

/** useReaderSearch 只读 store 的这几个字段/方法，给最小桩即可 */
function makeStore() {
  return {
    displayContent: '第一章正文里有锚这个词。',
    currentIndex: 0,
    currentChapter: { title: '第一章' },
    book: { bookId: 'book_1' },
    loadChapter: vi.fn(async () => undefined),
  } as never
}

describe('useReaderSearch 全书搜索', () => {
  beforeEach(() => {
    vi.mocked(searchCachedContent).mockReset()
  })

  it('全书模式走后端检索，按章聚合的 snippets 展平成逐条结果', async () => {
    vi.mocked(searchCachedContent).mockResolvedValue({
      results: [
        { chapterIndex: 2, chapterTitle: '第三章', chapterUrl: 'u3', snippets: ['…甲锚乙…', '…丙锚丁…'] },
        { chapterIndex: 5, chapterTitle: '第六章', chapterUrl: 'u6', snippets: ['…戊锚己…'] },
      ],
      cachedChapters: 12,
      totalChapters: 120,
      truncated: false,
    } as never)
    const search = useReaderSearch(makeStore())
    search.setSearchScope('book')
    search.searchQuery.value = '锚'
    search.runSearch()
    await vi.waitFor(() => {
      expect(search.bookSearchLoading.value).toBe(false)
    })

    expect(searchCachedContent).toHaveBeenCalledWith('book_1', '锚')
    expect(search.searchResults.value).toHaveLength(3)
    expect(search.searchResults.value[0].chapterIndex).toBe(2)
    expect(search.bookSearchCoverage.value).toEqual({
      cachedChapters: 12,
      totalChapters: 120,
      truncated: false,
    })
    expect(search.bookSearchStatus.value).toContain('12/120')
    expect(search.bookSearchStatus.value).toContain('3 条结果')
  })

  it('截断与零覆盖的状态文案如实标注', async () => {
    vi.mocked(searchCachedContent).mockResolvedValue({
      results: [],
      cachedChapters: 0,
      totalChapters: 0,
      truncated: false,
    } as never)
    const search = useReaderSearch(makeStore())
    search.setSearchScope('book')
    search.searchQuery.value = '词'
    search.runSearch()
    await vi.waitFor(() => expect(search.bookSearchLoading.value).toBe(false))
    expect(search.bookSearchStatus.value).toContain('没有已缓存的章节可供检索')
  })

  it('切范围清空结果；后端报错进状态文案', async () => {
    const search = useReaderSearch(makeStore())
    search.searchResults.value = [{ chapterIndex: 0, chapterTitle: 't', snippet: 's' }]
    search.setSearchScope('book')
    expect(search.searchResults.value).toEqual([])

    vi.mocked(searchCachedContent).mockRejectedValue(new Error('需要登录'))
    search.searchQuery.value = '锚'
    search.runSearch()
    await vi.waitFor(() => expect(search.bookSearchLoading.value).toBe(false))
    expect(search.bookSearchStatus.value).toBe('需要登录')
  })

  it('handleContentUpdated 只在当前章范围重跑', () => {
    const search = useReaderSearch(makeStore())
    search.showSearch.value = true
    search.searchQuery.value = '锚'
    search.setSearchScope('book')
    search.handleContentUpdated()
    // 全书范围不随翻章重跑（结果来自后端缓存，与当前章无关）
    expect(searchCachedContent).not.toHaveBeenCalled()
  })
})
