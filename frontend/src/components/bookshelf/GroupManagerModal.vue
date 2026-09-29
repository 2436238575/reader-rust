<template>
  <Teleport to="body">
    <Transition name="fade">
      <div v-if="modelValue" class="modal-overlay" @click="$emit('update:modelValue', false)"></div>
    </Transition>
    <Transition name="scale">
      <div
        v-if="modelValue"
        class="modal-container"
        @click.self="$emit('update:modelValue', false)"
      >
        <div class="modal-card">
          <div class="modal-header">
            <h3>分组管理</h3>
            <button class="close-btn" @click="$emit('update:modelValue', false)">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
            </button>
          </div>

          <div class="modal-body">
            <div class="create-row">
              <input
                v-model.trim="newGroupName"
                class="group-input"
                placeholder="新建分组名称"
                @keyup.enter="createGroup"
              />
              <button class="btn btn-primary" :disabled="!newGroupName" @click="createGroup">
                新建
              </button>
            </div>

            <div class="group-list">
              <div v-for="group in shelfStore.groups" :key="group.groupId" class="group-item">
                <input
                  v-model.trim="editingNames[group.groupId]"
                  class="group-input"
                  @keyup.enter="renameGroup(group.groupId)"
                />
                <div class="group-actions">
                  <button class="btn btn-sm" @click="renameGroup(group.groupId)">保存</button>
                  <button
                    class="btn btn-sm btn-danger"
                    @click="deleteGroup(group.groupId, group.groupName)"
                  >
                    删除
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { reactive, ref, watch, toRef } from 'vue'
import { useBookshelfStore } from '../../stores/bookshelf'
import { useAppStore } from '../../stores/app'
import { useEscClose } from '../../composables/useEscClose'

const props = defineProps<{
  modelValue: boolean
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
}>()

useEscClose(toRef(props, 'modelValue'), () => emit('update:modelValue', false))

const shelfStore = useBookshelfStore()
const appStore = useAppStore()
const newGroupName = ref('')
const editingNames = reactive<Record<number, string>>({})

watch(
  () => shelfStore.groups,
  (groups) => {
    groups.forEach((group) => {
      editingNames[group.groupId] = group.groupName
    })
  },
  { immediate: true, deep: true }
)

// 增改删都会整表刷新：请求未落地前挡住连点（连点可重复创建同名分组）
const groupWorking = ref(false)

async function createGroup() {
  if (!newGroupName.value || groupWorking.value) return
  groupWorking.value = true
  try {
    await shelfStore.saveGroup(newGroupName.value)
    newGroupName.value = ''
    appStore.showToast('分组已创建', 'success')
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '创建分组失败', 'error')
  } finally {
    groupWorking.value = false
  }
}

async function renameGroup(groupId: number) {
  const name = editingNames[groupId]?.trim()
  if (!name || groupWorking.value) return
  groupWorking.value = true
  try {
    await shelfStore.saveGroup(name, groupId)
    appStore.showToast('分组已更新', 'success')
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '更新分组失败', 'error')
  } finally {
    groupWorking.value = false
  }
}

async function deleteGroup(groupId: number, groupName: string) {
  if (groupWorking.value) return
  if (!confirm(`确定删除分组“${groupName}”？`)) return
  groupWorking.value = true
  try {
    await shelfStore.removeGroup(groupId)
    appStore.showToast('分组已删除', 'success')
  } catch (e: unknown) {
    appStore.showToast((e as Error).message || '删除分组失败', 'error')
  } finally {
    groupWorking.value = false
  }
}
</script>

<style scoped>
.modal-overlay {
  position: fixed;
  inset: 0;
  background: var(--overlay-mask-bg);
  -webkit-backdrop-filter: var(--overlay-mask-blur);
  backdrop-filter: var(--overlay-mask-blur);
  z-index: var(--z-overlay);
}

.modal-container {
  position: fixed;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: calc(24px + var(--safe-area-top)) calc(24px + var(--safe-area-right))
    calc(24px + var(--safe-area-bottom)) calc(24px + var(--safe-area-left));
  z-index: var(--z-modal);
}

.modal-card {
  width: min(560px, 100%);
  background: var(--color-bg-elevated);
  border-radius: 24px;
  box-shadow: var(--shadow-xl);
  overflow: hidden;
  max-height: calc(
    var(--app-height, 100dvh) - var(--safe-area-top) - var(--safe-area-bottom) - 32px
  );
  display: flex;
  flex-direction: column;
}

.modal-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 18px 20px;
  border-bottom: 1px solid var(--color-border-light);
}

.modal-header h3 {
  margin: 0;
  font-size: var(--text-lg);
}

.close-btn {
  width: 32px;
  height: 32px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 10px;
}

.close-btn svg {
  width: 18px;
  height: 18px;
}

.modal-body {
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  overflow-y: auto;
  -webkit-overflow-scrolling: touch;
}

.create-row,
.group-item {
  display: flex;
  gap: 10px;
}

.group-list {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.group-input {
  flex: 1;
  border: 1px solid var(--color-border);
  border-radius: 12px;
  padding: 10px 12px;
  background: var(--color-bg);
}

.group-actions {
  display: flex;
  gap: 8px;
}
</style>
