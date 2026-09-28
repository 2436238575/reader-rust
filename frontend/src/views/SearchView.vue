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
  if (source) {
    shelfStore.startSearch(q, { scope: 'source', sourceUrl: source })
    return
  }
  // URL 不带 source：别用默认值冲掉搜索页已有的范围/书源选择——
  // 否则保活返回时会先清空再修复选择，白白重发两轮 SSE 搜索
  if (shelfStore.searchKey !== q) {
    shelfStore.startSearch(q, {
      scope: shelfStore.searchScope,
      group: shelfStore.searchGroup,
      sourceUrl: shelfStore.searchSourceUrl,
    })
  }
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

// 搜索页被 keep-alive 缓存：离开不再丢状态（结果、滚动位置都在），
// 这里只在真正卸载（应用 teardown）时兜底清理
onUnmounted(() => {
  shelfStore.clearSearch()
})
</script>
