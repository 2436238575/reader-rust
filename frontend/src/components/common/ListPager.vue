<template>
  <nav v-if="shouldRender" class="list-pager" :class="{ 'with-info': hasInfo }" aria-label="分页">
    <div v-if="hasInfo" class="pager-info">
      <span class="pager-range">{{ rangeText }}</span>
      <label class="pager-size">
        <span>每页</span>
        <select :value="pageSize" @change="onSizeChange">
          <option v-for="size in pageSizes" :key="size" :value="size">{{ size }}</option>
        </select>
      </label>
    </div>
    <div v-if="totalPages > 1" class="pager-buttons">
      <button
        class="page-arrow"
        type="button"
        :disabled="page <= 1"
        aria-label="上一页"
        @click="go(page - 1)"
      >
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <path d="m15 18-6-6 6-6" />
        </svg>
      </button>
      <template v-for="(item, index) in items" :key="`${index}-${item}`">
        <span v-if="item === 'ellipsis'" class="page-gap">…</span>
        <button
          v-else
          class="page-num"
          :class="{ current: item === page }"
          type="button"
          :aria-current="item === page ? 'page' : undefined"
          @click="go(item)"
        >
          {{ item }}
        </button>
      </template>
      <button
        class="page-arrow"
        type="button"
        :disabled="page >= totalPages"
        aria-label="下一页"
        @click="go(page + 1)"
      >
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <path d="m9 18 6-6-6-6" />
        </svg>
      </button>
    </div>
  </nav>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { buildPageItems, clampPageOf, pageRangeText, pageCountOf } from '../../utils/listPager'

const props = defineProps<{
  /** 条目总数（组件内部换算总页数） */
  total: number
  pageSize: number
  page: number
  /** 提供后显示「N - M 共 T 个」区间文案与每页数量下拉；缺省或空数组不显示 */
  pageSizes?: number[]
}>()

const emit = defineEmits<{
  'update:page': [page: number]
  'update:pageSize': [size: number]
}>()

const totalPages = computed(() => pageCountOf(props.total, props.pageSize))
const items = computed(() => buildPageItems(totalPages.value, props.page))
const hasInfo = computed(() => (props.pageSizes?.length ?? 0) > 0)
// 信息区（总数/每页数量）本身有展示价值：单页时也渲染完整分页条
const shouldRender = computed(() => hasInfo.value || totalPages.value > 1)
const rangeText = computed(() => pageRangeText(props.total, props.pageSize, props.page))

function go(target: number) {
  const next = clampPageOf(target, totalPages.value)
  if (next !== props.page) {
    emit('update:page', next)
  }
}

function onSizeChange(event: Event) {
  const size = Number((event.target as HTMLSelectElement).value)
  if (Number.isFinite(size) && size > 0 && size !== props.pageSize) {
    emit('update:pageSize', size)
  }
}
</script>

<style scoped>
.list-pager {
  display: flex;
  align-items: center;
  justify-content: center;
  flex-wrap: wrap;
  gap: 4px;
  flex-shrink: 0;
}

/* 完整分页条：区间与每页数量靠左，页码按钮靠右 */
.list-pager.with-info {
  justify-content: space-between;
  gap: 8px 12px;
  padding: 12px 14px;
  border-top: 1px solid var(--color-border-light);
}

.pager-info {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 10px;
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.pager-size {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.pager-size select {
  min-height: 28px;
  padding: 0 var(--space-3);
  border: 1px solid var(--color-border);
  border-radius: 999px;
  background: transparent;
  color: var(--color-text-secondary);
  font-size: var(--text-xs);
  cursor: pointer;
}

.pager-buttons {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 4px;
}

.page-num,
.page-arrow {
  min-width: 32px;
  height: 32px;
  padding: 0 6px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--radius-md);
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
  transition:
    background var(--duration-fast) var(--ease-out),
    color var(--duration-fast) var(--ease-out);
}

.page-num:hover,
.page-arrow:hover:not(:disabled) {
  background: var(--color-bg-hover);
  color: var(--color-text);
}

.page-num.current {
  background: var(--color-primary);
  color: var(--color-text-inverse);
  font-weight: 600;
}

.page-num:disabled,
.page-arrow:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

.page-arrow svg {
  width: 16px;
  height: 16px;
}

.page-gap {
  color: var(--color-text-tertiary);
  padding: 0 2px;
}
</style>
