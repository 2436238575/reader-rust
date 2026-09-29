<script setup lang="ts">
// 图片放大预览：正文配图与评论配图共用。点任意处关闭。
import { ref, watch } from 'vue'

const props = defineProps<{ src: string }>()
const emit = defineEmits<{ (e: 'close'): void }>()

// 图片走书源管道可能较慢或失败：黑罩上至少要有加载中/失败提示，而不是一块纯黑
const loading = ref(true)
const failed = ref(false)

function onImgError() {
  loading.value = false
  failed.value = true
}

watch(
  () => props.src,
  () => {
    loading.value = true
    failed.value = false
  }
)
</script>

<template>
  <Teleport to="body">
    <div v-if="src" class="image-lightbox" @click="emit('close')">
      <div v-if="failed" class="lightbox-hint">图片加载失败</div>
      <div v-else-if="loading" class="lightbox-hint">
        <span class="lightbox-spinner"></span>加载中...
      </div>
      <img
        :src="src"
        alt=""
        referrerpolicy="no-referrer"
        :class="{ 'is-ready': !loading && !failed }"
        @load="loading = false"
        @error="onImgError"
      />
    </div>
  </Teleport>
</template>

<style scoped>
.image-lightbox {
  position: fixed;
  inset: 0;
  /* 终止级查看器：盖在评论面板（--z-modal）与 toast 之上 */
  z-index: var(--z-lightbox);
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.88);
  cursor: zoom-out;
}

.image-lightbox img {
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
  opacity: 0;
  transition: opacity 0.2s;
}

.image-lightbox img.is-ready {
  opacity: 1;
}

.lightbox-hint {
  position: absolute;
  display: flex;
  align-items: center;
  gap: 8px;
  color: rgba(255, 255, 255, 0.85);
  font-size: 14px;
}

.lightbox-spinner {
  width: 16px;
  height: 16px;
  border: 2px solid rgba(255, 255, 255, 0.25);
  border-top-color: rgba(255, 255, 255, 0.85);
  border-radius: 50%;
  animation: lightbox-spin 1s linear infinite;
}

@keyframes lightbox-spin {
  to {
    transform: rotate(360deg);
  }
}
</style>
