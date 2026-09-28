<script setup lang="ts">
// 图片放大预览：正文配图与评论配图共用。点任意处关闭。
defineProps<{ src: string }>()
const emit = defineEmits<{ (e: 'close'): void }>()
</script>

<template>
  <Teleport to="body">
    <div v-if="src" class="image-lightbox" @click="emit('close')">
      <img :src="src" alt="" referrerpolicy="no-referrer" />
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
}
</style>
