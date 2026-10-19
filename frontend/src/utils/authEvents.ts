// need-login 事件的统一派发点（带 1.5s 去抖，并发 401 不会弹一排登录框）。
// http.ts 拦截器与不走 axios 的裸 fetch（openaiSpeech 等）都用它。

let lastDispatchAt = 0

export function notifyNeedLogin() {
  const now = Date.now()
  if (now - lastDispatchAt < 1500) return
  lastDispatchAt = now
  window.dispatchEvent(new CustomEvent('need-login'))
}
