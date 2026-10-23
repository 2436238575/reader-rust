import { notifyNeedLogin } from './authEvents'
import { summarizeHttpErrorBody } from './httpError'
import { API_BASE } from './appBase'
import { buildAuthHeaders as buildReaderJwtHeaders } from './secureAccess'
import {
  RefreshNetworkError,
  SessionExpiredError,
  refreshAccessToken,
} from './authSession'

export const DEFAULT_OPENAI_BASE_URL = 'http://localhost:8825'

export function normalizeOpenAIBaseUrl(url: string) {
  return url.trim().replace(/\/+$/, '')
}

export function buildOpenAISpeechUrl(baseUrl: string) {
  return `${normalizeOpenAIBaseUrl(baseUrl)}/v1/audio/speech`
}

export interface OpenAISpeechRequest {
  source?: 'browser' | 'server'
  baseUrl: string
  apiKey?: string
  input: string
  model: string
  voice: string
  format?: string
  speed?: number
  signal?: AbortSignal
}

function buildAuthHeaders(apiKey?: string) {
  const headers: Record<string, string> = {}
  if (!apiKey?.trim()) return headers
  headers.Authorization = `Bearer ${apiKey.trim()}`
  return headers
}

async function readSpeechError(response: Response) {
  const fallback = `语音请求失败 (${response.status})`
  const contentType = response.headers.get('content-type') || ''

  try {
    if (contentType.includes('application/json')) {
      const data = (await response.json()) as {
        error?: {
          message?: string
        }
      }
      return data.error?.message || fallback
    }

    const text = (await response.text()).trim()
    return summarizeHttpErrorBody(text, { fallback, status: response.status })
  } catch {
    return fallback
  }
}

export async function requestOpenAISpeechAudio({
  source = 'browser',
  baseUrl,
  apiKey,
  input,
  model,
  voice,
  format,
  speed,
  signal,
}: OpenAISpeechRequest) {
  const body = {
    model,
    input,
    voice,
    response_format: format || 'mp3',
    speed,
  }
  const serverRequest = source === 'server'
  const requestInit = (): RequestInit => ({
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...(serverRequest ? buildReaderJwtHeaders() : buildAuthHeaders(apiKey)),
    },
    body: JSON.stringify(
      serverRequest
        ? { useServerConfig: true, kind: 'speech', path: '/v1/audio/speech', body }
        : body
    ),
    signal,
  })
  const url = serverRequest ? `${API_BASE}/aiProxy` : buildOpenAISpeechUrl(baseUrl)

  let response = await fetch(url, requestInit())
  let refreshNetworkFailure = false
  if (response.status === 401 && serverRequest) {
    // 裸 fetch 不经过 axios 拦截器：静默换一次 Access Token 再重试。
    // 刷新因断网失败时保留原响应，不要把用户登出。
    try {
      await refreshAccessToken()
      response = await fetch(url, requestInit())
    } catch (error) {
      refreshNetworkFailure =
        error instanceof RefreshNetworkError
      // SessionExpiredError：refreshAccessToken 已触发登录跳转
      if (error instanceof SessionExpiredError) {
        refreshNetworkFailure = false
      }
    }
  }

  if (!response.ok) {
    // 直连第三方 API 的 401 是对方 API Key 的问题，与本地会话无关
    if (response.status === 401 && serverRequest && !refreshNetworkFailure) {
      notifyNeedLogin()
    }
    throw new Error(await readSpeechError(response))
  }

  return response.blob()
}
