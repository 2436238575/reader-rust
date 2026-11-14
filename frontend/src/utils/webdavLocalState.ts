/**
 * WebDAV 备份里的浏览器本地配置：采集与落地都只认白名单。
 *
 * 备份文件是用户可上传的输入，不能把任意键写进 localStorage（会覆盖会话
 * hint 等非备份数据）；旧版前端也只读这几个键。
 */
export const WEBDAV_LOCAL_STATE_KEYS = [
  'theme',
  'reader-stats',
  'readConfig',
  'reader-themeIndex',
  'reader-isNight',
  'reader-speechConfig',
  'reader-last-session',
  'reader-currentIndex',
  'reader-source-subscriptions',
] as const

/** 采集当前浏览器里存在的白名单键（服务端不接触 localStorage）。 */
export function captureWebdavLocalState(): Record<string, string> {
  const state: Record<string, string> = {}
  for (const key of WEBDAV_LOCAL_STATE_KEYS) {
    const value = localStorage.getItem(key)
    if (value != null) {
      state[key] = value
    }
  }
  return state
}

/**
 * 应用备份里的 localStorage 快照。
 *
 * `null`/`undefined` = 这份备份不含本地配置（旧备份/手写文档），**不改动**
 * 现有键；显式空对象才按整份覆盖（备份时这些键确实都没有值）。
 */
export function applyWebdavLocalState(state: Record<string, string> | null | undefined) {
  if (state == null) {
    return
  }
  WEBDAV_LOCAL_STATE_KEYS.forEach((key) => {
    const value = state[key]
    if (value == null) {
      localStorage.removeItem(key)
    } else {
      localStorage.setItem(key, value)
    }
  })
}
