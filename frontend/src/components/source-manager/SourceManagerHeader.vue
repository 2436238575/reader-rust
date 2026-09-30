<template>
  <header class="source-manager-header">
    <button class="icon-btn back-btn" type="button" title="返回" @click="$emit('back')">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <path d="M19 12H5" />
        <path d="M12 19l-7-7 7-7" />
      </svg>
    </button>
    <div class="title-block">
      <h2>书源管理</h2>
      <p>
        共 {{ total }} 个 · 启用 {{ enabled }} 个 · 当前筛选 {{ filtered }} 个
        <span v-if="selected > 0"> · 已选 {{ selected }} 个</span>
      </p>
    </div>
    <div class="header-actions">
      <button
        class="icon-btn"
        :class="{ spinning: loading }"
        type="button"
        title="刷新"
        @click="$emit('refresh')"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
          <path d="M3 3v5h5" />
          <path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16" />
          <path d="M16 16h5v5" />
        </svg>
      </button>
      <button class="btn" type="button" @click="$emit('import-local')">本地导入</button>
      <button class="btn" type="button" @click="$emit('open-subscriptions')">远程同步</button>
      <button class="btn" type="button" @click="$emit('export')">导出</button>
      <button
        class="btn"
        :disabled="testing || total === 0"
        type="button"
        @click="$emit('test-sources')"
      >
        {{ testing ? '测试中...' : '测试书源' }}
      </button>
      <button
        class="btn btn-danger"
        :disabled="testing || invalidCount === 0"
        type="button"
        @click="$emit('delete-invalid')"
      >
        删除失效<span v-if="invalidCount"> {{ invalidCount }}</span>
      </button>
      <button class="btn btn-primary" type="button" @click="$emit('create')">新增</button>
    </div>
  </header>
</template>

<script setup lang="ts">
defineProps<{
  total: number
  enabled: number
  filtered: number
  selected: number
  loading: boolean
  testing: boolean
  invalidCount: number
}>()

defineEmits<{
  refresh: []
  'import-local': []
  'open-subscriptions': []
  export: []
  'test-sources': []
  'delete-invalid': []
  create: []
  back: []
}>()
</script>

<style scoped>
.source-manager-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: var(--space-5) 0;
  border-bottom: 1px solid var(--color-border-light);
  flex-shrink: 0;
}

.title-block {
  flex: 1;
  min-width: 0;
}

.title-block h2 {
  font-size: var(--text-lg);
  font-weight: 700;
}

.title-block p {
  margin-top: 6px;
  font-size: var(--text-sm);
  color: var(--color-text-tertiary);
}

.header-actions {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: var(--space-2);
}

/* 图标按钮与其他页面统一：无边框，悬停出底色 */
.icon-btn {
  border-radius: var(--radius-md);
  border: none;
  background: transparent;
  cursor: pointer;
  transition: all var(--duration-fast);
}

.icon-btn {
  width: 36px;
  height: 36px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--color-text-secondary);
  flex-shrink: 0;
}

.icon-btn svg {
  width: 18px;
  height: 18px;
}

.icon-btn:hover {
  background: var(--color-bg-hover);
}

.icon-btn.spinning svg {
  animation: spin 1s linear infinite;
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

@media (max-width: 640px) {
  .source-manager-header {
    position: relative;
    flex-direction: column;
    align-items: stretch;
    padding: var(--space-4) 0;
  }

  .title-block {
    /* 给左上角的返回按钮让位 */
    padding-left: 44px;
  }

  .header-actions {
    width: 100%;
    justify-content: flex-start;
  }

  .back-btn {
    position: absolute;
    top: var(--space-4);
    left: 0;
  }
}
</style>
