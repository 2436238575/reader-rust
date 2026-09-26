/**
 * 部署前缀。
 *
 * 默认根路径部署（`/`）；用 nginx 反代到非根路径时，构建前设置
 * `VITE_BASE_PATH=/read/`，产物里所有静态资源与接口地址都会带上前缀。
 * 例如：`location /read/ { proxy_pass http://127.0.0.1:8080/; }`
 * （末尾斜杠会把 `/read/` 前缀剥掉，后端仍按根路径处理）。
 */
export const APP_BASE = import.meta.env.BASE_URL || '/'

/** 接口基址（axios / EventSource / 裸 fetch 共用）。 */
export const API_BASE = `${APP_BASE.replace(/\/+$/, '')}/reader3`

/**
 * 给应用内的绝对路径套上部署前缀。
 *
 * 需要过这里的有两类：服务端返回的路径（封面 `/reader3/image/<id>`、
 * 本地书资源 `/reader3/localEpubAsset`）与手工拼的接口地址。
 * 已经是完整 URL 或相对路径的原样返回。
 */
export function withAppBase(path: string) {
  if (!path.startsWith('/')) return path
  if (APP_BASE === '/') return path
  return `${APP_BASE.replace(/\/+$/, '')}${path}`
}
