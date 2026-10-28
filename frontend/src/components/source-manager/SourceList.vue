<template>
  <section class="source-list-wrapper">
    <div v-if="loading" class="loading-skeleton" aria-busy="true" aria-label="书源加载中">
      <div v-for="i in 8" :key="i" class="skeleton-row">
        <SkeletonBlock width="16px" height="16px" radius="4px" />
        <div class="skeleton-row-lines">
          <SkeletonBlock width="34%" height="14px" />
          <SkeletonBlock width="58%" height="12px" />
        </div>
      </div>
    </div>
    <div v-else-if="!sources.length" class="empty-state">
      <strong>{{ emptyTitle }}</strong>
      <span>{{ emptyDescription }}</span>
    </div>
    <template v-else>
      <!-- 数千书源按页渲染（每页数量见下方分页条），行数少不再需要虚拟滚动 -->
      <div ref="listEl" class="source-list">
        <article
          v-for="source in pageSources"
          :key="source.bookSourceId ?? source.bookSourceUrl"
          class="source-item"
          :class="{
            active: activeId === source.bookSourceId,
            selected: source.bookSourceId != null && selectedIds.has(source.bookSourceId),
          }"
        >
          <label class="row-check" @click.stop>
            <input
              type="checkbox"
              :checked="source.bookSourceId != null && selectedIds.has(source.bookSourceId)"
              @change="$emit('toggle-selection', source)"
            />
          </label>
          <button class="source-info" type="button" @click="$emit('edit', source)">
            <span class="source-name">{{ source.bookSourceName }}</span>
            <span class="source-url">{{ source.bookSourceUrl }}</span>
            <span v-if="source.bookSourceGroup" class="source-group">{{
              source.bookSourceGroup
            }}</span>
          </button>
          <div class="source-actions">
            <label class="toggle" title="启用状态">
              <input
                type="checkbox"
                :checked="source.enabled !== false"
                @change="$emit('toggle-enabled', source)"
              />
              <span class="toggle-slider"></span>
            </label>
            <button
              class="icon-btn small danger"
              type="button"
              title="删除"
              @click="$emit('delete', source)"
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path
                  d="M3 6h18M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"
                />
              </svg>
            </button>
          </div>
        </article>
      </div>
      <ListPager
        v-model:page="pageModel"
        v-model:page-size="pageSizeModel"
        :total="sources.length"
        :page-sizes="[20, 50, 100]"
      />
    </template>
  </section>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import SkeletonBlock from '../common/SkeletonBlock.vue'
import ListPager from '../common/ListPager.vue'
import type { BookSource } from '../../types'

const props = defineProps<{
  sources: BookSource[]
  loading: boolean
  selectedIds: ReadonlySet<number>
  activeId?: number
  emptyTitle: string
  emptyDescription: string
  page: number
  pageSize: number
}>()

const emit = defineEmits<{
  edit: [source: BookSource]
  'toggle-enabled': [source: BookSource]
  'toggle-selection': [source: BookSource]
  delete: [source: BookSource]
  'update:page': [page: number]
  'update:pageSize': [size: number]
}>()

const listEl = ref<HTMLElement | null>(null)

// 页码与每页数量由父组件持有（筛选/换每页数量后的归位也由父组件处理）；
// 这里只负责在列表变短后把越界页码收回来
const pageModel = computed({
  get: () => props.page,
  set: (value: number) => emit('update:page', value),
})

const pageSizeModel = computed({
  get: () => props.pageSize,
  set: (value: number) => emit('update:pageSize', value),
})

const pageSources = computed(() => {
  const start = (props.page - 1) * props.pageSize
  return props.sources.slice(start, start + props.pageSize)
})

watch(
  [() => props.sources.length, () => props.pageSize],
  ([length, size], [, previousSize]) => {
    // 换每页数量的归位走父组件（重置到第 1 页），跳过避免两个方向互相覆盖
    if (size !== previousSize) return
    const pages = Math.max(1, Math.ceil(length / size))
    if (props.page > pages) {
      pageModel.value = pages
    }
  }
)

// 翻页后从新页的开头看起，不带着上一页的滚动位置
watch(
  () => props.page,
  () => {
    if (listEl.value) {
      listEl.value.scrollTop = 0
    }
  }
)
</script>

<style scoped>
.source-list-wrapper {
  min-height: 0;
  border: 1px solid var(--color-border-light);
  border-radius: 14px;
  overflow: hidden;
  display: flex;
  flex-direction: column;
  background: var(--color-bg-elevated);
}

.source-list {
  flex: 1;
  overflow: auto;
}

.source-item {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  align-items: center;
  gap: 12px;
  padding: 13px 14px;
  border-bottom: 1px solid var(--color-border-light);
  transition:
    background var(--duration-fast) var(--ease-out),
    box-shadow var(--duration-fast) var(--ease-out);
}

.source-item.active {
  background: color-mix(in srgb, var(--color-primary) 6%, transparent);
}

.source-item.selected {
  background: color-mix(in srgb, var(--color-primary) 10%, transparent);
  box-shadow: inset 3px 0 0 var(--color-primary);
}

.row-check {
  display: inline-flex;
  align-items: center;
}

.row-check input {
  width: 16px;
  height: 16px;
  accent-color: var(--color-primary);
}

.source-info {
  min-width: 0;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 4px;
  text-align: left;
  cursor: pointer;
}

.source-name {
  max-width: 100%;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.source-url {
  max-width: 100%;
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
  overflow-wrap: anywhere;
}

.source-group {
  max-width: 100%;
  padding: 2px 8px;
  border-radius: var(--radius-full);
  background: var(--color-bg-hover);
  color: var(--color-text-secondary);
  font-size: var(--text-xs);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.source-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.toggle {
  position: relative;
  display: inline-flex;
  width: 38px;
  height: 22px;
}

.toggle input {
  opacity: 0;
  width: 0;
  height: 0;
}

.toggle-slider {
  position: absolute;
  inset: 0;
  border-radius: var(--radius-full);
  background: var(--color-border);
  transition: background var(--duration-fast);
}

.toggle-slider::before {
  content: '';
  position: absolute;
  width: 18px;
  height: 18px;
  left: 2px;
  top: 2px;
  border-radius: 50%;
  background: var(--color-text-inverse);
  box-shadow: var(--shadow-xs);
  transition: transform var(--duration-fast);
}

.toggle input:checked + .toggle-slider {
  background: var(--color-primary);
}

.toggle input:checked + .toggle-slider::before {
  transform: translateX(16px);
}

.icon-btn {
  width: 30px;
  height: 30px;
  display: flex;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  color: var(--color-text-secondary);
  transition: all var(--duration-fast);
}

.icon-btn:hover {
  background: var(--color-bg-hover);
}

.icon-btn svg {
  width: 14px;
  height: 14px;
}

.icon-btn.danger:hover {
  background: color-mix(in srgb, var(--color-danger) 8%, transparent);
  color: var(--color-danger);
}

.empty-state {
  flex: 1;
  min-height: 260px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  padding: 24px;
  text-align: center;
  color: var(--color-text-tertiary);
}

.empty-state strong {
  color: var(--color-text-secondary);
}

/* 骨架行贴近真实书源行形态：左勾选块 + 右两行（名称/URL） */
.skeleton-row {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  align-items: center;
  gap: 12px;
  padding: 13px 14px;
  border-bottom: 1px solid var(--color-border-light);
}

.skeleton-row-lines {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

@media (max-width: 640px) {
  .source-item {
    grid-template-columns: auto minmax(0, 1fr);
  }

  .source-actions {
    grid-column: 2;
    justify-content: flex-start;
  }
}
</style>
