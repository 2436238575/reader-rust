<script setup lang="ts">
// 图片放大预览：正文配图与评论配图共用。
// 滚轮/双击缩放（以光标为锚点），单指拖动（放大后），双指捏合（移动端）；
// 点击关闭仅在「没有拖动过」时触发，拖完松手不会误关。
import { computed, ref, watch } from 'vue'
import {
  clampLightboxScale,
  LIGHTBOX_DRAG_THRESHOLD_PX,
  normalizeLightboxZoom,
  pinchScaleAt,
  zoomTranslateAt,
  type Point,
} from '../../utils/lightboxZoom'

const props = defineProps<{ src: string }>()
const emit = defineEmits<{ (e: 'close'): void }>()

// 图片走书源管道可能较慢或失败：黑罩上至少要有加载中/失败提示，而不是一块纯黑
const loading = ref(true)
const failed = ref(false)

const scale = ref(1)
const translate = ref<Point>({ x: 0, y: 0 })
/** 有指针按着（拖动/捏合进行中）：关 transition 跟手，也用来抑制松手后的 click 关闭 */
const interacting = ref(false)

const pointers = new Map<number, Point>()
let dragStart: (Point & { tx: number; ty: number }) | null = null
let pinchStart: { dist: number; scale: number } | null = null
let moved = false

function onImgError() {
  loading.value = false
  failed.value = true
}

function applyZoom(nextScale: number, nextTranslate: Point) {
  const normalized = normalizeLightboxZoom(clampLightboxScale(nextScale), nextTranslate)
  scale.value = normalized.scale
  translate.value = normalized.translate
}

function onWheel(event: WheelEvent) {
  event.preventDefault()
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  const center = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 }
  const nextScale = clampLightboxScale(scale.value * (event.deltaY < 0 ? 1.15 : 1 / 1.15))
  const nextTranslate = zoomTranslateAt(
    { x: event.clientX, y: event.clientY },
    center,
    translate.value,
    scale.value,
    nextScale
  )
  applyZoom(nextScale, nextTranslate)
}

function onDblClick(event: MouseEvent) {
  event.preventDefault()
  if (scale.value > 1) {
    applyZoom(1, translate.value)
    return
  }
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  const center = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 }
  const nextScale = 2.5
  const nextTranslate = zoomTranslateAt(
    { x: event.clientX, y: event.clientY },
    center,
    translate.value,
    scale.value,
    nextScale
  )
  applyZoom(nextScale, nextTranslate)
}

function onPointerDown(event: PointerEvent) {
  ;(event.currentTarget as HTMLElement).setPointerCapture?.(event.pointerId)
  pointers.set(event.pointerId, { x: event.clientX, y: event.clientY })
  interacting.value = true
  moved = false
  if (pointers.size === 2) {
    const [a, b] = [...pointers.values()]
    pinchStart = { dist: Math.hypot(a.x - b.x, a.y - b.y), scale: scale.value }
    dragStart = null
  } else if (pointers.size === 1) {
    pinchStart = null
    dragStart = {
      x: event.clientX,
      y: event.clientY,
      tx: translate.value.x,
      ty: translate.value.y,
    }
  }
}

function onPointerMove(event: PointerEvent) {
  if (!pointers.has(event.pointerId)) return
  pointers.set(event.pointerId, { x: event.clientX, y: event.clientY })

  if (pinchStart && pointers.size === 2) {
    const [a, b] = [...pointers.values()]
    const dist = Math.hypot(a.x - b.x, a.y - b.y)
    applyZoom(pinchScaleAt(pinchStart.scale, pinchStart.dist, dist), translate.value)
    moved = true
    return
  }
  if (!dragStart) return
  const dx = event.clientX - dragStart.x
  const dy = event.clientY - dragStart.y
  if (Math.hypot(dx, dy) > LIGHTBOX_DRAG_THRESHOLD_PX) moved = true
  // 未放大时没有可拖动的余量（位移会在 normalize 时归零），直接忽略
  if (scale.value > 1 && moved) {
    translate.value = { x: dragStart.tx + dx, y: dragStart.ty + dy }
  }
}

function onPointerUp(event: PointerEvent) {
  pointers.delete(event.pointerId)
  if (pointers.size < 2) pinchStart = null
  if (pointers.size === 0) {
    dragStart = null
    interacting.value = false
  }
}

function onClick() {
  // 拖动/捏合结束会紧跟一个 click：松手不应当成「点击关闭」
  if (moved) {
    moved = false
    return
  }
  emit('close')
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') emit('close')
}

const imageStyle = computed(() => ({
  transform: `translate(${translate.value.x}px, ${translate.value.y}px) scale(${scale.value})`,
  transition: interacting.value ? 'none' : 'transform 0.15s ease-out, opacity 0.2s',
  cursor: scale.value > 1 ? (interacting.value ? 'grabbing' : 'grab') : 'zoom-out',
}))

watch(
  () => props.src,
  (src) => {
    loading.value = true
    failed.value = false
    applyZoom(1, { x: 0, y: 0 })
    pointers.clear()
    dragStart = null
    pinchStart = null
    if (src) {
      window.addEventListener('keydown', onKeydown)
    } else {
      window.removeEventListener('keydown', onKeydown)
    }
  }
)
</script>

<template>
  <Teleport to="body">
    <div
      v-if="src"
      class="image-lightbox"
      @click="onClick"
      @wheel="onWheel"
      @dblclick="onDblClick"
      @pointerdown="onPointerDown"
      @pointermove="onPointerMove"
      @pointerup="onPointerUp"
      @pointercancel="onPointerUp"
    >
      <div v-if="failed" class="lightbox-hint">图片加载失败</div>
      <div v-else-if="loading" class="lightbox-hint">
        <span class="lightbox-spinner"></span>加载中...
      </div>
      <img
        :src="src"
        alt=""
        referrerpolicy="no-referrer"
        draggable="false"
        :class="{ 'is-ready': !loading && !failed }"
        :style="imageStyle"
        @load="loading = false"
        @error="onImgError"
      />
      <div v-if="!loading && !failed" class="lightbox-tip">滚轮/双击缩放 · 拖动查看 · ESC 关闭</div>
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
  /* 缩放/拖动手势全由组件接管，禁止浏览器默认的页面捏合缩放与触摸滚动 */
  touch-action: none;
  user-select: none;
}

.image-lightbox img {
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
  opacity: 0;
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

.lightbox-tip {
  position: absolute;
  bottom: 18px;
  left: 50%;
  transform: translateX(-50%);
  color: rgba(255, 255, 255, 0.45);
  font-size: 12px;
  pointer-events: none;
  white-space: nowrap;
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
