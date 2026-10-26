<template>
  <div v-if="lineList" class="skeleton-lines" :style="{ gap }" aria-hidden="true">
    <div
      v-for="(lineWidth, index) in lineList"
      :key="index"
      class="skeleton-block"
      :style="{ width: lineWidth, height, borderRadius: radius }"
    ></div>
  </div>
  <div
    v-else
    class="skeleton-block"
    :style="{ width, height, borderRadius: radius }"
    aria-hidden="true"
  ></div>
</template>

<script setup lang="ts">
import { computed } from 'vue'

/**
 * 基础骨架块：加载占位用。样式全部走 token，深色模式自适应。
 * 单块传 width / height / radius；段落状多行传 lines（每项是行宽百分比）。
 * 骨架是纯装饰，对辅助技术隐藏；「加载中」语义由外层容器的 aria-busy 表达。
 */
const props = withDefaults(
  defineProps<{
    width?: string
    height?: string
    radius?: string
    /** 行宽百分比数组，如 [100, 100, 72] 渲染三行，末行 72% 宽 */
    lines?: number[]
    /** lines 模式下的行间距 */
    gap?: string
  }>(),
  {
    width: '100%',
    height: '14px',
    radius: 'var(--radius-sm)',
    lines: undefined,
    gap: '12px',
  }
)

const lineList = computed(() => props.lines?.map((w) => `${w}%`) ?? null)
</script>

<style scoped>
.skeleton-lines {
  display: flex;
  flex-direction: column;
}

.skeleton-block {
  background: var(--color-bg-active);
  animation: skeleton-pulse 1.6s var(--ease-in-out) infinite;
}

@keyframes skeleton-pulse {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.45;
  }
}

@media (prefers-reduced-motion: reduce) {
  .skeleton-block {
    animation: none;
  }
}
</style>
