import http from './http'

/**
 * 读取当前用户的 JSON 文档（最近阅读、阅读统计等需要跨浏览器同步的数据）。
 * 对应后端 /reader3/getUserdata；文档不存在时返回 null。
 */
export function getUserdata<T>(name: string) {
  return http
    .get<unknown>(`/getUserdata?name=${encodeURIComponent(name)}`)
    .then((r) => (r.data ?? null) as T | null)
}

/**
 * 整文档覆盖写当前用户的 JSON 文档。
 * 对应后端 /reader3/saveUserdata。
 */
export function saveUserdata(name: string, value: unknown) {
  return http.post('/saveUserdata', { name, value }).then(() => undefined)
}
