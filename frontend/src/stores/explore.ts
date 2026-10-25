import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { exploreBook } from '../api/explore'
import { useSourceStore } from './source'
import type { SearchBook, BookSource } from '../types'
import {
  getInitialExploreCategoryUrl,
  parseExploreCategories,
  type ExploreCategory,
} from '../utils/exploreCategories'

export const useExploreStore = defineStore('explore', () => {
  const sourceStore = useSourceStore()

  const activeSourceId = ref<number | null>(null)
  const activeCategoryUrl = ref<string>('')

  const books = ref<SearchBook[]>([])
  const loading = ref(false)
  const page = ref(1)
  const hasMore = ref(true)
  const error = ref<string | null>(null)

  // 筛选出启用了 explore 的书源
  const exploreSources = computed(() => {
    return sourceStore.sources.filter((s: BookSource) => s.enabledExplore && s.exploreUrl)
  })

  // 当前选中的书源对象
  const currentSource = computed(() => {
    return sourceStore.sources.find((s: BookSource) => s.bookSourceId === activeSourceId.value)
  })

  // 解析当前书源的 exploreUrl 分类
  const categories = computed<ExploreCategory[]>(() => {
    return parseExploreCategories(currentSource.value?.exploreUrl)
  })

  function ensureActiveSource() {
    if (exploreSources.value.length === 0) {
      activeSourceId.value = null
      activeCategoryUrl.value = ''
      books.value = []
      hasMore.value = false
      return
    }

    const activeSourceStillValid = exploreSources.value.some(
      (source) => source.bookSourceId === activeSourceId.value
    )
    if (activeSourceId.value == null || !activeSourceStillValid) {
      const first = exploreSources.value[0]
      if (first.bookSourceId != null) setSource(first.bookSourceId)
      return
    }

    if (!categories.value.some((category) => category.url === activeCategoryUrl.value)) {
      const firstCategoryUrl = getInitialExploreCategoryUrl(categories.value)
      if (firstCategoryUrl) {
        setCategory(firstCategoryUrl)
      }
    }
  }

  function setSource(id: number) {
    const sourceChanged = activeSourceId.value !== id
    if (sourceChanged) {
      activeSourceId.value = id
    }

    const firstCategoryUrl = getInitialExploreCategoryUrl(categories.value)
    if (!firstCategoryUrl) {
      activeCategoryUrl.value = ''
      books.value = []
      hasMore.value = false
      return
    }

    const activeCategoryStillValid = categories.value.some(
      (category) => category.url === activeCategoryUrl.value
    )
    if (sourceChanged || !activeCategoryStillValid) {
      setCategory(firstCategoryUrl)
    }
  }

  function setCategory(url: string) {
    const nextUrl = url.trim()
    if (!nextUrl) return
    if (activeCategoryUrl.value !== nextUrl) {
      activeCategoryUrl.value = nextUrl
      resetAndFetch()
    }
  }

  async function resetAndFetch() {
    books.value = []
    page.value = 1
    hasMore.value = true
    error.value = null
    // 有在途请求也要发起新请求：旧响应回来时被代际检查丢弃
    await fetchMore(true)
  }

  // 请求代际：响应回来时若已切换分类/书源（有更新的请求），直接丢弃，
  // 避免加载中切分类时旧分类数据混入新分类的空列表
  let fetchSeq = 0

  async function fetchMore(force = false) {
    if (
      (!force && loading.value) ||
      !hasMore.value ||
      activeSourceId.value == null ||
      !activeCategoryUrl.value
    )
      return

    const seq = ++fetchSeq
    const sourceId = activeSourceId.value
    const categoryUrl = activeCategoryUrl.value

    loading.value = true
    error.value = null
    try {
      const result = await exploreBook({
        bookSourceId: sourceId,
        ruleFindUrl: categoryUrl,
        page: page.value,
      })
      if (seq !== fetchSeq) return

      if (result && result.length > 0) {
        books.value.push(...result)
        page.value++
      } else {
        hasMore.value = false
      }
    } catch (err: any) {
      if (seq !== fetchSeq) return
      error.value = err.message || '加载失败'
      // 停住自动翻页等用户点重试；error 态在视图里优先于「没有更多了」展示
      hasMore.value = false
    } finally {
      // 过期请求的 finally 不能灭掉新请求的 loading
      if (seq === fetchSeq) loading.value = false
    }
  }

  /** 错误态的重试入口：恢复翻页条件后重拉当前页 */
  function retryFetch() {
    if (loading.value) return
    hasMore.value = true
    error.value = null
    void fetchMore()
  }

  // 初始化时加载书源数据
  async function init() {
    error.value = null
    try {
      // 快照占位的书源没有 exploreUrl，发现页必须等全量就位再选源
      if (sourceStore.partial || sourceStore.sources.length === 0) {
        await sourceStore.ensureFullSources()
      }
    } catch (err: any) {
      // 书源列表都拉不到时把错误摆到内容区（带重试），而不是抛给 onMounted
      // 变成 unhandled rejection + 一句误导的「无带有发现规则的书源」
      error.value = err?.message || '书源加载失败'
      return
    }
    ensureActiveSource()
  }

  // 注意：这里刻意不 watch(exploreSources)。store 一旦被实例化（如顶栏拼搜索链接），
  // 全局 watch 就会在任何页面拉到书源列表后自动选中第一个源并发起 exploreBook 请求。
  // 书源列表变化的对账由发现页在激活期间自行观察（见 ExploreView 的 onActivated/onDeactivated）。

  return {
    activeSourceId,
    activeCategoryUrl,
    books,
    loading,
    page,
    hasMore,
    error,
    exploreSources,
    currentSource,
    categories,
    init,
    ensureActiveSource,
    setSource,
    setCategory,
    fetchMore,
    retryFetch,
    resetAndFetch,
  }
})
