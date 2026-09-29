<template>
  <div v-if="relationshipGraph.nodes.length" class="graph-fallback">
    <div class="graph-canvas">
      <div class="graph-legend">
        <span class="legend-location">地点</span>
        <span class="legend-character">角色</span>
      </div>
      <svg
        :viewBox="`0 0 ${graphLayout.width} ${graphLayout.height}`"
        role="img"
        aria-label="人物关系图"
      >
        <defs>
          <marker
            id="graph-arrow"
            viewBox="0 0 10 10"
            refX="8"
            refY="5"
            markerWidth="5"
            markerHeight="5"
            orient="auto-start-reverse"
          >
            <path d="M 0 0 L 10 5 L 0 10 z" />
          </marker>
        </defs>
        <g class="graph-links">
          <g
            v-for="link in graphLayout.links"
            :key="`${link.source}-${link.target}-${link.label}`"
            class="graph-link"
            :class="{
              highlighted: link.highlighted,
              dimmed: link.dimmed,
              located: link.label === '位于',
            }"
          >
            <path :d="link.path" />
            <g v-if="link.showLabel" class="graph-link-label">
              <rect
                :x="link.labelX - graphLabelWidth(link.label) / 2"
                :y="link.labelY - 13"
                :width="graphLabelWidth(link.label)"
                height="22"
                rx="11"
              />
              <text :x="link.labelX" :y="link.labelY + 3">{{ link.label }}</text>
            </g>
          </g>
        </g>
        <g
          v-for="node in graphLayout.nodes"
          :key="node.id"
          class="graph-node"
          :class="{
            active: selectedGraphNode?.id === node.id,
            location: node.kind === 'location',
            dimmed: node.dimmed,
            connected: node.connectedToSelected,
          }"
          @click="selectGraphNode(node.id)"
        >
          <foreignObject :x="node.x" :y="node.y" :width="node.width" :height="node.height">
            <div xmlns="http://www.w3.org/1999/xhtml" class="graph-node-card">
              <span class="node-dot"></span>
              <strong>{{ node.label }}</strong>
            </div>
          </foreignObject>
        </g>
      </svg>
    </div>
    <aside v-if="selectedGraphNode" class="graph-detail">
      <div class="graph-detail-head">
        <span>{{ selectedGraphNode.kind === 'location' ? '地点' : '角色' }}</span>
        <strong>{{ selectedGraphNode.label }}</strong>
      </div>
      <p>{{ selectedGraphNode.detail || '暂无说明' }}</p>
      <div v-if="selectedGraphConnections.length" class="graph-connection-list">
        <span>直接关联</span>
        <button
          v-for="connection in selectedGraphConnections"
          :key="`${connection.id}-${connection.relation}`"
          @click="selectGraphNode(connection.id)"
        >
          <strong>{{ connection.label }}</strong>
          <small>{{ connection.relation }}</small>
        </button>
      </div>
      <small>{{ fallbackReason || '图片地图未生成，显示关系图' }}</small>
    </aside>
  </div>
  <div v-else class="map-empty">暂无地图</div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import type { AiBookMemory } from '../../types'
import {
  buildAiBookRelationshipGraph,
  layoutAiBookRelationshipGraph,
} from '../../utils/aiBookGraph'

const props = defineProps<{
  memory: AiBookMemory | null
  fallbackReason?: string
}>()

const selectedGraphNodeId = ref('')

const relationshipGraph = computed(() =>
  props.memory ? buildAiBookRelationshipGraph(props.memory) : { nodes: [], links: [] }
)
const activeGraphNodeId = computed(() => {
  const selected = selectedGraphNodeId.value
  if (selected && relationshipGraph.value.nodes.some((node) => node.id === selected)) {
    return selected
  }
  return relationshipGraph.value.nodes[0]?.id || ''
})
const graphLayout = computed(() =>
  layoutAiBookRelationshipGraph(relationshipGraph.value, activeGraphNodeId.value)
)
const selectedGraphNode = computed(() => {
  return (
    graphLayout.value.nodes.find((node) => node.id === activeGraphNodeId.value) ||
    graphLayout.value.nodes[0] ||
    null
  )
})
const selectedGraphConnections = computed(() => {
  const current = selectedGraphNode.value
  if (!current) return []
  return graphLayout.value.links
    .filter((link) => link.source === current.id || link.target === current.id)
    .map((link) => {
      const otherId = link.source === current.id ? link.target : link.source
      const other = graphLayout.value.nodes.find((node) => node.id === otherId)
      return other ? { id: other.id, label: other.label, relation: link.label } : null
    })
    .filter((item): item is { id: string; label: string; relation: string } => Boolean(item))
})

function selectGraphNode(id: string) {
  selectedGraphNodeId.value = id
}

function graphLabelWidth(label: string) {
  return Math.max(44, Math.min(108, label.length * 14 + 22))
}
</script>

<style scoped>
.graph-fallback {
  width: 100%;
  min-height: 500px;
  display: grid;
  grid-template-columns: minmax(0, 1fr) 280px;
  background: var(--color-bg-elevated);
}

.graph-canvas {
  min-height: 500px;
  display: flex;
  align-items: stretch;
  position: relative;
  background:
    linear-gradient(rgba(70, 134, 121, 0.045) 1px, transparent 1px),
    linear-gradient(90deg, rgba(70, 134, 121, 0.045) 1px, transparent 1px),
    radial-gradient(circle at 50% 50%, rgba(212, 129, 42, 0.07), transparent 38%),
    var(--color-bg-elevated);
  background-size:
    28px 28px,
    28px 28px,
    100% 100%,
    auto;
}

.graph-legend {
  position: absolute;
  top: 14px;
  left: 18px;
  z-index: 1;
  display: inline-flex;
  gap: 8px;
  padding: 6px;
  border: 1px solid var(--color-border-light);
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.76);
  backdrop-filter: blur(10px);
}

.graph-legend span {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-height: 24px;
  padding: 0 10px;
  border-radius: 999px;
  color: var(--color-text-secondary);
  font-size: var(--text-xs);
  font-weight: 700;
}

.graph-legend span::before {
  content: '';
  width: 8px;
  height: 8px;
  border-radius: 999px;
}

.legend-location::before {
  background: #468679;
}

.legend-character::before {
  background: var(--color-primary);
}

.graph-canvas svg {
  width: 100%;
  height: auto;
  min-height: 500px;
}

.graph-link path {
  fill: none;
  stroke: rgba(52, 61, 56, 0.18);
  stroke-width: 2;
  marker-end: url(#graph-arrow);
  transition:
    opacity var(--duration-fast),
    stroke var(--duration-fast),
    stroke-width var(--duration-fast);
}

.graph-link.located path {
  stroke-dasharray: 5 8;
}

.graph-link.highlighted path {
  stroke: rgba(212, 129, 42, 0.72);
  stroke-width: 3;
}

.graph-link.dimmed {
  opacity: 0.18;
}

.graph-link marker path,
marker#graph-arrow path {
  fill: rgba(52, 61, 56, 0.35);
}

.graph-link text {
  fill: var(--color-text-secondary);
  font-size: var(--text-xs);
  font-weight: 700;
  text-anchor: middle;
}

.graph-link-label rect {
  fill: rgba(255, 255, 255, 0.88);
  stroke: var(--color-border-light);
}

.graph-node {
  cursor: pointer;
  transition: opacity var(--duration-fast);
}

.graph-node.dimmed {
  opacity: 0.28;
}

.graph-node-card {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 13px;
  border: 2px solid rgba(212, 129, 42, 0.62);
  border-radius: 999px;
  background: rgba(255, 247, 238, 0.94);
  color: var(--color-text);
  box-shadow: 0 10px 24px rgba(140, 120, 90, 0.1);
  overflow: hidden;
  transition:
    border-color var(--duration-fast),
    background var(--duration-fast),
    color var(--duration-fast),
    box-shadow var(--duration-fast),
    transform var(--duration-fast);
}

.graph-node-card strong {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
  line-height: 1.2;
}

.node-dot {
  flex: 0 0 auto;
  width: 9px;
  height: 9px;
  border-radius: 999px;
  background: var(--color-primary);
}

.graph-node.location .graph-node-card {
  border-color: rgba(70, 134, 121, 0.72);
  background: rgba(235, 247, 244, 0.94);
}

.graph-node.location .node-dot {
  background: #468679;
}

.graph-node.connected .graph-node-card {
  box-shadow:
    0 10px 24px rgba(140, 120, 90, 0.12),
    0 0 0 4px rgba(212, 129, 42, 0.08);
}

.graph-node.active .graph-node-card {
  transform: translateY(-1px);
  border-color: var(--color-primary);
  background: var(--color-primary);
  color: #fff;
  box-shadow:
    0 14px 30px rgba(212, 129, 42, 0.24),
    0 0 0 5px rgba(212, 129, 42, 0.14);
}

.graph-node.active .node-dot {
  background: rgba(255, 255, 255, 0.86);
}

.graph-detail {
  border-left: 1px solid var(--color-border-light);
  padding: 18px;
  display: flex;
  flex-direction: column;
  justify-content: center;
  gap: 14px;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.7), rgba(255, 255, 255, 0)), var(--color-bg);
}

.graph-detail-head {
  display: grid;
  gap: 6px;
}

.graph-detail span,
.graph-detail small {
  color: var(--color-text-tertiary);
  font-size: var(--text-xs);
}

.graph-detail strong {
  font-size: var(--text-lg);
  line-height: 1.35;
}

.graph-detail p {
  margin: 0;
  line-height: 1.7;
  color: var(--color-text-secondary);
}

.graph-connection-list {
  display: grid;
  gap: 8px;
}

.graph-connection-list > span {
  color: var(--color-text-tertiary);
  font-size: var(--text-xs);
}

.graph-connection-list button {
  min-width: 0;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 9px 10px;
  border: 1px solid var(--color-border-light);
  border-radius: 8px;
  background: var(--color-bg-elevated);
  text-align: left;
  transition:
    border-color var(--duration-fast),
    background var(--duration-fast);
}

.graph-connection-list button:hover {
  border-color: var(--color-primary-border);
  background: var(--color-primary-bg);
}

.graph-connection-list button strong {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-sm);
}

.graph-connection-list button small {
  flex: 0 0 auto;
}

.map-empty {
  color: var(--color-text-tertiary);
  padding: 48px;
  text-align: center;
}

@media (max-width: 767px) {
  .graph-fallback {
    grid-template-columns: 1fr;
  }

  .graph-detail {
    border-left: 0;
    border-top: 1px solid var(--color-border-light);
  }
}
</style>
