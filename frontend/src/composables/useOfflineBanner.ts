import { computed, ref } from 'vue'
import type { useReaderStore } from '../stores/reader'
import type { useAppStore } from '../stores/app'
import { countBrowserBookCache } from '../utils/browserCache'

type ReaderStore = ReturnType<typeof useReaderStore>
type AppStore = ReturnType<typeof useAppStore>

/// 离线横幅：断网时告诉用户当前书有多少章已缓存进浏览器可读。
/// 计数只在本章内容/索引变化后由调用方 schedule 刷新（防抖合并）。
export function useOfflineBanner(store: ReaderStore, appStore: AppStore) {
  const offlineCachedCount = ref(0)

  const offlineBannerText = computed(() => {
    if (appStore.isOnline) return ''
    if (offlineCachedCount.value > 0) {
      return `离线模式：当前书已缓存 ${offlineCachedCount.value} 章，可继续阅读已缓存章节`
    }
    return '离线模式：当前书尚未缓存到浏览器，未缓存章节将无法打开'
  })

  async function refreshOfflineCacheState() {
    if (!store.book?.bookId) {
      offlineCachedCount.value = 0
      return
    }
    offlineCachedCount.value = await countBrowserBookCache(store.book.bookId).catch(() => 0)
  }

  let refreshTimer: number | null = null
  function scheduleRefreshOfflineCacheState() {
    if (refreshTimer) clearTimeout(refreshTimer)
    refreshTimer = window.setTimeout(() => {
      refreshTimer = null
      void refreshOfflineCacheState()
    }, 120)
  }

  function disposeOfflineBanner() {
    if (refreshTimer) {
      clearTimeout(refreshTimer)
      refreshTimer = null
    }
  }

  return {
    offlineCachedCount,
    offlineBannerText,
    refreshOfflineCacheState,
    scheduleRefreshOfflineCacheState,
    disposeOfflineBanner,
  }
}
