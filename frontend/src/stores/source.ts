import { defineStore } from 'pinia'
import { ref, shallowRef } from 'vue'
import { getBookSources } from '../api/source'
import type { BookSource } from '../types'

export const useSourceStore = defineStore('source', () => {
  // 书源规则 JSON 可达 MB 级且按整体替换更新：浅响应式避免深代理几百个源
  const sources = shallowRef<BookSource[]>([])
  const loading = ref(false)
  let loadingTask: Promise<void> | null = null

  async function fetchSources() {
    if (loadingTask) return loadingTask
    loading.value = true
    loadingTask = getBookSources()
      .then((list) => {
        sources.value = list
      })
      .finally(() => {
        loading.value = false
        loadingTask = null
      })
    return loadingTask
  }

  return {
    sources,
    loading,
    fetchSources
  }
})
