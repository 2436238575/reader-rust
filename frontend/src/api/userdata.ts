import http from './http'

// ─── 首屏批量预读缓存 ───
// 登录后由 stores/app.ts 调 prefetchUserdata 一次性批量拉取首屏文档（文档名常量
// 见 utils/userdataDocs），随后各业务模块的 getUserdata 命中缓存直接消费，
// 省掉冷启动的 N 次单文档往返。短 TTL + 一次性消费 + 写后失效，避免长期陈旧。
const PREFETCH_TTL_MS = 30_000

interface PrefetchEntry {
  value: unknown
  expiresAt: number
}

const prefetchCache = new Map<string, PrefetchEntry>()

/**
 * 批量预读 userdata 文档：一次 getUserdataBatch 把给定文档写入短期缓存。
 * fire-and-forget 用途：失败静默（业务随后会走单文档 GET 兜底）。
 * 保持简单：每次都发一次批量请求并覆盖对应缓存，不去重已缓存的 name。
 */
export async function prefetchUserdata(names: string[]): Promise<void> {
  if (!names.length) return
  try {
    const docs = await getUserdataBatch(names)
    const expiresAt = Date.now() + PREFETCH_TTL_MS
    for (const name of names) {
      prefetchCache.set(name, { value: docs[name] ?? null, expiresAt })
    }
  } catch {
    // 离线/未登录：不缓存，后续 getUserdata 走单文档 GET
  }
}

/** 清空预读缓存：登出/会话失效时由 stores/app.ts 调用 */
export function clearUserdataPrefetch() {
  prefetchCache.clear()
}

/**
 * 读取当前用户的 JSON 文档（最近阅读、阅读统计等需要跨浏览器同步的数据）。
 * 命中预读缓存且未过期时直接返回并删除该条（一次性消费，避免长期陈旧）；
 * 否则走单文档 GET。对应后端 /reader3/getUserdata；文档不存在时返回 null。
 */
export function getUserdata<T>(name: string) {
  const cached = prefetchCache.get(name)
  if (cached) {
    // 无论是否过期都只消费一次：过期的条目顺手清掉再回落单文档 GET
    prefetchCache.delete(name)
    if (cached.expiresAt > Date.now()) {
      return Promise.resolve(cached.value as T | null)
    }
  }
  return http
    .get<unknown>(`/getUserdata?name=${encodeURIComponent(name)}`)
    .then((r) => (r.data ?? null) as T | null)
}

/**
 * 整文档覆盖写当前用户的 JSON 文档。
 * 对应后端 /reader3/saveUserdata。
 */
export function saveUserdata(name: string, value: unknown) {
  return http.post('/saveUserdata', { name, value }).then(() => {
    // 写成功后预读缓存已陈旧，删除之；写失败保留缓存（值仍有效）
    prefetchCache.delete(name)
  })
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
  return http.post('/saveUserdataBatch', { docs }).then(() => {
    for (const doc of docs) prefetchCache.delete(doc.name)
  })
}
