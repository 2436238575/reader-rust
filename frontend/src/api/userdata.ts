import http from './http'

// ─── 首屏批量预读缓存 ───
// 登录后由 stores/app.ts 调 prefetchUserdata 一次性批量拉取首屏文档（文档名常量
// 见 utils/userdataDocs），随后各业务模块的 getUserdataBatch 命中缓存直接消费，
// 省掉冷启动的重复往返。短 TTL + 一次性消费 + 写后失效，避免长期陈旧。
const PREFETCH_TTL_MS = 30_000

interface PrefetchEntry {
  value: unknown
  expiresAt: number
}

const prefetchCache = new Map<string, PrefetchEntry>()

/**
 * 批量预读 userdata 文档：一次 getUserdataBatch 把给定文档写入短期缓存。
 * fire-and-forget 用途：失败静默（业务随后的 getUserdataBatch 会重新拉取）。
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
    // 离线/未登录：不缓存，后续 getUserdataBatch 照常发请求
  }
}

/** 清空预读缓存：登出/会话失效时由 stores/app.ts 调用 */
export function clearUserdataPrefetch() {
  prefetchCache.clear()
}

/**
 * 按文档名精确失效预读缓存（不清整表）：写成功后调用。
 * 除 saveUserdataBatch 外，utils/userdataQueue 的 keepalive 直发路径也走这里——
 * 它不经 saveUserdataBatch 封装，需要单独同步失效，且只动本地缓存、不发请求。
 */
export function invalidateUserdataPrefetch(names: string[]) {
  for (const name of names) prefetchCache.delete(name)
}

/**
 * 批量读取当前用户的 JSON 文档（一次 ≤20 个），把首屏的 N 次读取合成一次
 * 往返；单文档读取传单元素数组即可。命中预读缓存且未过期的条目直接返回
 * （一次性消费，避免长期陈旧），只对 miss/过期的条目发请求。
 * 对应后端 /reader3/getUserdataBatch?names=a,b,c；
 * 缺失的文档在返回映射里为 null。
 */
export function getUserdataBatch<T = unknown>(names: string[]) {
  const now = Date.now()
  const hits: Record<string, T | null> = {}
  const misses: string[] = []
  for (const name of names) {
    const cached = prefetchCache.get(name)
    // 无论是否过期都只消费一次：过期的条目顺手清掉再随 miss 重新拉取
    if (cached) {
      prefetchCache.delete(name)
      if (cached.expiresAt > now) {
        hits[name] = cached.value as T | null
        continue
      }
    }
    misses.push(name)
  }
  if (!misses.length) return Promise.resolve(hits)
  const query = misses.map((name) => encodeURIComponent(name)).join(',')
  return http.get<unknown>(`/getUserdataBatch?names=${query}`).then((r) => {
    const docs = (r.data as { docs?: Record<string, T | null> } | null)?.docs ?? {}
    return { ...docs, ...hits }
  })
}

/**
 * 批量整文档覆盖写当前用户的 JSON 文档（一次 ≤20 个）：
 * 任一条目非法整批不落库；同名文档按最后一次为准。
 * 对应后端 /reader3/saveUserdataBatch；前端写入统一经 utils/userdataQueue 收口到这里。
 */
export function saveUserdataBatch(docs: { name: string; value: unknown }[]) {
  return http.post('/saveUserdataBatch', { docs }).then(() => {
    // 写成功后预读缓存已陈旧，删除之；写失败保留缓存（值仍有效）
    invalidateUserdataPrefetch(docs.map((doc) => doc.name))
  })
}
