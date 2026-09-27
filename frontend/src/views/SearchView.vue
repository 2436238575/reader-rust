<template>
  <SearchResults @back="goBack" />
</template>

<script setup lang="ts">
import { onUnmounted, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import SearchResults from '../components/SearchResults.vue'
import { useBookshelfStore } from '../stores/bookshelf'

const route = useRoute()
const router = useRouter()
const shelfStore = useBookshelfStore()

// 搜索页以 URL 为唯一事实来源（#/search?q=...）；
// 范围筛选（全部/分组/单源）仍是会话内状态，不进 URL
function applyQuery() {
  if (route.path !== '/search') return
  const q = typeof route.query.q === 'string' ? route.query.q.trim() : ''
  if (!q) {
    router.replace('/')
    return
  }
  const source = typeof route.query.source === 'string' ? route.query.source : ''
  shelfStore.startSearch(q, source ? { scope: 'source', sourceUrl: source } : {})
}

function goBack() {
  // 正常进入时返回上一页；直达链接（无历史记录）兜底回书架
  if (window.history.state?.back != null) {
    router.back()
  } else {
    router.replace('/')
  }
}

watch(() => route.fullPath, applyQuery, { immediate: true })

// 离开搜索页即丢弃搜索状态；再次进入时由 URL 重新发起
onUnmounted(() => {
  shelfStore.clearSearch()
})
</script>
