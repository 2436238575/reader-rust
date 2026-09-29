import http from './http'
import type { AiServerModelStatus } from '../types'

/**
 * 后端 AI 模型的可用状态（只有布尔值）。
 *
 * 配置本体只存在于服务端 env（`AI_TEXT_*` / `AI_IMAGE_*` / `AI_SPEECH_*`），
 * 从不下发到浏览器；也没有保存接口——改配置只能改服务端环境变量。
 */
export function getAiModelStatus() {
  return http.get<AiServerModelStatus>('/getAiModelConfig').then((r) => r.data)
}
