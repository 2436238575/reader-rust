<template>
  <Teleport to="body">
    <Transition name="fade">
      <div v-if="modelValue" class="modal-overlay" @click="close"></div>
    </Transition>
    <Transition :name="isMobileLayout ? 'slide-right' : 'scale'">
      <div v-if="modelValue" class="modal-container" @click.self="close">
        <div class="ai-settings-modal">
          <div class="modal-head">
            <div>
              <h2>AI 设置</h2>
              <p>AI资料、AI 地图与听书语音的模型配置</p>
            </div>
            <button class="close-btn" @click="close" aria-label="关闭">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
            </button>
          </div>

          <div class="modal-body">
            <section class="settings-card">
              <div class="settings-card-head">
                <h3>后端模型状态</h3>
              </div>
              <ul class="server-status-list">
                <li>
                  文本模型（AI资料）
                  <span :class="['status-pill', { ready: aiStore.serverTextReady }]">
                    {{ aiStore.serverTextReady ? '已配置可用' : '未配置' }}
                  </span>
                </li>
                <li>
                  图片模型（AI 地图）
                  <span :class="['status-pill', { ready: aiStore.serverImageReady }]">
                    {{ aiStore.serverImageReady ? '已配置可用' : '未配置' }}
                  </span>
                </li>
                <li>
                  语音模型（听书）
                  <span :class="['status-pill', { ready: aiStore.serverSpeechReady }]">
                    {{ aiStore.serverSpeechReady ? '已配置可用' : '未配置' }}
                  </span>
                </li>
                <li>
                  AI 资料编排（sidecar）
                  <span :class="['status-pill', { ready: aiStore.agentReady }]">
                    {{ aiStore.agentReady ? '已就绪' : '未就绪' }}
                  </span>
                </li>
              </ul>
              <p class="settings-hint">
                AI资料与地图由服务端编排生成，模型配置在服务端环境变量维护
                （AI_TEXT_* / AI_IMAGE_*），改配置需要重启后端；此处只读。
              </p>
            </section>

            <section class="settings-card">
              <div class="settings-card-head">
                <h3>听书语音（OpenAI Speech）</h3>
              </div>
              <div class="source-options">
                <button
                  class="source-option"
                  :class="{ active: speechCfg.openaiSource === 'browser' }"
                  @click="readerStore.setOpenAISpeechSource('browser')"
                >
                  浏览器
                </button>
                <button
                  class="source-option"
                  :class="{ active: speechCfg.openaiSource === 'server' }"
                  :disabled="statusLoaded && !aiStore.serverSpeechReady"
                  @click="selectServerSpeech"
                >
                  后端
                </button>
              </div>

              <div v-if="speechCfg.openaiSource === 'server'" class="server-note">
                <span :class="['status-pill', { ready: aiStore.serverSpeechReady }]">
                  {{ aiStore.serverSpeechReady ? '已配置可用' : '未配置' }}
                </span>
                <p class="settings-hint">
                  由服务端 env 的 AI_SPEECH_* 维护；请求经后端代理转发，浏览器接触不到 API Key。
                </p>
              </div>

              <div v-else class="settings-grid speech-grid">
                <label class="field span-2">
                  <span>服务地址</span>
                  <input
                    :value="speechCfg.openaiBaseUrl"
                    type="url"
                    placeholder="http://localhost:8825"
                    @input="
                      readerStore.setOpenAISpeechBaseUrl(($event.target as HTMLInputElement).value)
                    "
                  />
                </label>
                <label class="field span-2">
                  <span>API Key</span>
                  <input
                    :value="speechCfg.openaiApiKey"
                    type="password"
                    placeholder="sk-..."
                    autocomplete="off"
                    @input="
                      readerStore.setOpenAISpeechApiKey(($event.target as HTMLInputElement).value)
                    "
                  />
                </label>
                <label class="field">
                  <span>语音模型</span>
                  <input
                    :value="speechCfg.openaiModel"
                    placeholder="gpt-4o-mini-tts"
                    @input="
                      readerStore.setOpenAISpeechModel(($event.target as HTMLInputElement).value)
                    "
                  />
                </label>
                <label class="field">
                  <span>语音音色</span>
                  <input
                    :value="speechCfg.openaiVoice"
                    placeholder="alloy"
                    @input="
                      readerStore.setOpenAISpeechVoice(($event.target as HTMLInputElement).value)
                    "
                  />
                </label>
                <label class="field">
                  <span>音频格式</span>
                  <select
                    :value="speechCfg.openaiFormat"
                    @change="
                      readerStore.setOpenAISpeechFormat(
                        ($event.target as HTMLSelectElement).value as
                          'mp3' | 'wav' | 'opus' | 'flac' | 'pcm'
                      )
                    "
                  >
                    <option value="mp3">mp3</option>
                    <option value="wav">wav</option>
                    <option value="opus">opus</option>
                    <option value="flac">flac</option>
                    <option value="pcm">pcm</option>
                  </select>
                </label>
                <div class="field">
                  <span>请求模式</span>
                  <div class="source-options request-mode">
                    <button
                      class="source-option"
                      :class="{ active: speechCfg.openaiRequestMode === 'chunked' }"
                      @click="readerStore.setOpenAISpeechRequestMode('chunked')"
                    >
                      少字多请求
                    </button>
                    <button
                      class="source-option"
                      :class="{ active: speechCfg.openaiRequestMode === 'merged' }"
                      @click="readerStore.setOpenAISpeechRequestMode('merged')"
                    >
                      多字少请求
                    </button>
                  </div>
                </div>
                <p class="settings-hint span-2">
                  少字多请求会按短句细分并预加载更多片段；多字少请求会合并较短段落，只预加载下一段。
                  URL 和 Key 仅保存在当前浏览器。
                </p>
              </div>
            </section>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import { computed, ref, toRef, watch } from 'vue'
import { useAiBookStore } from '../stores/aiBook'
import { useAppStore } from '../stores/app'
import { useReaderStore } from '../stores/reader'
import { useEscClose } from '../composables/useEscClose'
import { useMobileLayout } from '../composables/useMobileLayout'

const props = defineProps<{ modelValue: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [boolean] }>()

const aiStore = useAiBookStore()
const appStore = useAppStore()
const readerStore = useReaderStore()
const { isMobileLayout } = useMobileLayout()

const speechCfg = computed(() => readerStore.speechConfig)
const statusLoaded = ref(false)

function close() {
  emit('update:modelValue', false)
}

useEscClose(toRef(props, 'modelValue'), close)

watch(
  () => props.modelValue,
  async (open) => {
    if (!open) return
    // 每次打开都拉最新后端状态（可能在别处改过）
    statusLoaded.value = false
    await aiStore.loadServerModelStatus({ force: true })
    statusLoaded.value = true
  }
)

async function selectServerSpeech() {
  if (!statusLoaded.value) {
    await aiStore.loadServerModelStatus({ force: true })
    statusLoaded.value = true
  }
  if (!aiStore.serverSpeechReady) {
    appStore.showToast('后端未启用语音模型，请在服务端 env 设置 AI_SPEECH_*', 'warning')
    return
  }
  readerStore.setOpenAISpeechSource('server')
}
</script>

<style scoped>
.modal-overlay {
  position: fixed;
  inset: 0;
  background: var(--overlay-mask-bg);
  backdrop-filter: var(--overlay-mask-blur);
  -webkit-backdrop-filter: var(--overlay-mask-blur);
  z-index: var(--z-overlay);
}

.modal-container {
  position: fixed;
  inset: 0;
  z-index: var(--z-modal);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}

.ai-settings-modal {
  background: var(--color-bg-elevated);
  border-radius: 16px;
  width: min(640px, 100%);
  max-height: min(84vh, 760px);
  display: flex;
  flex-direction: column;
  border: 1px solid var(--color-border);
  box-shadow: var(--shadow-lg);
}

.modal-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 18px 20px 12px;
  border-bottom: 1px solid var(--color-border);
}

.modal-head h2 {
  margin: 0;
  font-size: var(--text-lg);
}

.modal-head p {
  margin: 4px 0 0;
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.close-btn {
  padding: 6px;
  border-radius: 8px;
  color: var(--color-text-secondary);
  display: inline-flex;
}

.close-btn:hover {
  background: var(--color-bg-hover);
}

.close-btn svg {
  width: 18px;
  height: 18px;
}

.modal-body {
  padding: 12px 20px;
  overflow-y: auto;
  display: grid;
  gap: 10px;
}

/* 模型来源行：不占整张卡片，标签在左、分段选择在右 */
.source-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 2px 2px 0;
}

.source-row-label {
  font-size: var(--text-sm);
  font-weight: 600;
}

.source-hint {
  margin-top: -2px;
  padding: 0 2px;
}


.settings-card {
  background: var(--color-bg-sunken);
  border: 1px solid var(--color-border);
  border-radius: 12px;
  padding: 10px 14px;
}

.settings-card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 6px;
}

.settings-card-head h3 {
  margin: 0;
  font-size: var(--text-sm);
}

.settings-hint {
  margin: 10px 0 0;
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
  line-height: 1.6;
}

.source-options {
  display: inline-flex;
  gap: 6px;
  padding: 4px;
  border-radius: 8px;
  background: var(--color-bg);
}

.source-option {
  padding: 6px 18px;
  border-radius: 6px;
  color: var(--color-text-secondary);
  transition: all 0.15s;
}

.source-option.active {
  background: var(--color-bg-elevated);
  color: var(--color-text);
  font-weight: 600;
  box-shadow: var(--shadow-sm);
}

.source-option:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

.server-status-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: grid;
  gap: 8px;
}

.server-status-list li {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: var(--text-sm);
  color: var(--color-text-secondary);
}

.status-pill {
  font-size: var(--text-xs);
  padding: 2px 10px;
  border-radius: 999px;
  background: var(--color-bg);
  color: var(--color-text-tertiary);
}

.status-pill.ready {
  background: rgba(82, 157, 82, 0.16);
  color: var(--color-success, #529d52);
}

.settings-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 8px;
}

.settings-grid > .settings-hint {
  grid-column: span 2;
}

.field {
  display: grid;
  gap: 4px;
}

.field.span-2 {
  grid-column: span 2;
}

.field span {
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.field input,
.field select {
  padding: 8px 10px;
  border-radius: 8px;
  border: 1px solid var(--color-border);
  background: var(--color-bg);
  color: var(--color-text);
  font-size: var(--text-sm);
}

.field input:focus,
.field select:focus {
  outline: none;
  border-color: var(--color-primary);
}

.switch-line {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: var(--text-sm);
  color: var(--color-text-secondary);
  cursor: pointer;
}

.switch-line.compact {
  font-size: var(--text-xs);
}

.switch-line input {
  display: none;
}

.switch-ui {
  width: 32px;
  height: 18px;
  border-radius: 999px;
  background: var(--color-bg);
  border: 1px solid var(--color-border);
  position: relative;
  transition: background 0.15s;
  flex: none;
}

.switch-ui::after {
  content: '';
  position: absolute;
  top: 2px;
  left: 2px;
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: var(--color-text-tertiary);
  transition: all 0.15s;
}

.switch-line input:checked + .switch-ui {
  background: var(--color-primary);
  border-color: var(--color-primary);
}

.switch-line input:checked + .switch-ui::after {
  left: 16px;
  background: #fff;
}

.proxy-option {
  padding: 4px 2px;
}

.speech-grid {
  margin-top: 12px;
}

.request-mode {
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-border);
}

.request-mode .source-option {
  padding: 5px 12px;
  font-size: var(--text-xs);
}

.request-mode .source-option.active {
  background: var(--color-bg-sunken);
}

.server-note {
  margin-top: 12px;
  display: flex;
  flex-direction: column;
  gap: 4px;
  align-items: flex-start;
}

@media (max-width: 767px) {
  .modal-overlay {
    display: none;
  }

  .modal-container {
    padding: 0;
  }

  .ai-settings-modal {
    width: 100%;
    max-height: none;
    height: 100%;
    border-radius: 0;
    border: none;
  }

  .modal-head {
    padding-top: calc(14px + var(--safe-area-top));
  }
}
</style>
