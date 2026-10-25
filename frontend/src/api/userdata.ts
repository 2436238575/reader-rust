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

/**
 * 批量读取当前用户的 JSON 文档（一次 ≤20 个），把首屏的 N 次单文档 GET
 * 合成一次往返。缺失的文档在返回映射里为 null。
 * 对应后端 /reader3/getUserdataBatch?names=a,b,c。
 */
export function getUserdataBatch<T = unknown>(names: string[]) {
  const query = names.map((name) => encodeURIComponent(name)).join(',')
  return http
    .get<unknown>(`/getUserdataBatch?names=${query}`)
    .then((r) => {
      const docs = (r.data as { docs?: Record<string, T | null> } | null)?.docs
      return docs ?? {}
    })
}

/**
 * 批量整文档覆盖写当前用户的 JSON 文档（一次 ≤20 个）：
 * 任一条目非法整批不落库；同名文档按最后一次为准。
 * 对应后端 /reader3/saveUserdataBatch；前端写入统一经 utils/userdataQueue 收口到这里。
 */
export function saveUserdataBatch(docs: { name: string; value: unknown }[]) {
  return http.post('/saveUserdataBatch', { docs }).then(() => undefined)
}
