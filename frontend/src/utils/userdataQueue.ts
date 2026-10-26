/**
 * userdata 批量写队列。
 *
 * 阅读统计、最近阅读、已读标记等模块各自把整文档推给后端；这里把同一
 * 窗口内的写按文档名合并（同名后写覆盖前写），200ms 防抖后合成一次
 * /reader3/saveUserdataBatch，并保持单飞（同一时刻只有一个批量在途）。
 * 失败时未发送成功的文档留在队列里，按退避间隔自动重试（至多 3 次，
 * 不无限重试；下一次写入会重开一轮重试预算）。
 * 成功清理按版本号判断是否被新写覆盖：每次入队分配递增版本，发送时
 * 捕获版本，落地回调只删版本未变的条目——消费方（阅读统计）入队的是
 * 同一个活对象并继续原地修改，按引用相等会把在途期间的新值误删。
 *
 * 页面卸载（pagehide）或切入后台（visibilitychange=hidden）时用裸
 * fetch + keepalive 补发最后一次批量写——axios 不支持 keepalive，卸载
 * 阶段普通请求会被浏览器取消。URL 经 API_BASE 拼接，跟随子路径部署。
 */
import { invalidateUserdataPrefetch, saveUserdataBatch } from '../api/userdata'
import { API_BASE } from './appBase'

const DEBOUNCE_MS = 200
// 失败后的退避重试间隔；用完即放弃，等下一次写入再试
const RETRY_DELAYS_MS = [1000, 2000, 4000]

interface PendingDoc {
  value: unknown
  /** 入队时分配的递增版本：成功清理据此判断发送后是否又被新写覆盖 */
  version: number
}

const pending = new Map<string, PendingDoc>()
let nextVersion = 0
let debounceTimer: ReturnType<typeof setTimeout> | null = null
let retryTimer: ReturnType<typeof setTimeout> | null = null
let retryAttempts = 0
let inFlight: Promise<void> | null = null

/** 把一个文档的整量新值排入写队列；同名文档后写覆盖前写。 */
export function queueUserdataSave(name: string, value: unknown) {
  nextVersion += 1
  pending.set(name, { value, version: nextVersion })
  // 新写入重开一轮重试预算，并取代已排期的退避重试（200ms 防抖比退避更快）
  retryAttempts = 0
  if (retryTimer) {
    clearTimeout(retryTimer)
    retryTimer = null
  }
  scheduleFlush(DEBOUNCE_MS)
}

function scheduleFlush(delay: number) {
  if (debounceTimer) clearTimeout(debounceTimer)
  debounceTimer = setTimeout(() => {
    debounceTimer = null
    void flushUserdataQueue().catch(() => undefined)
  }, delay)
}

/**
 * 立即把当前待写文档合成一次批量写发出；返回的 Promise 在发送完成后 settle
 * （失败时 reject，同时文档留在队列里等退避重试，调用方自行兜底）。
 * 单飞：已有批量在途时，等它结束后对剩余待写文档再补一轮。
 */
export function flushUserdataQueue(): Promise<void> {
  if (debounceTimer) {
    clearTimeout(debounceTimer)
    debounceTimer = null
  }
  if (inFlight) {
    // 失败路径已由在途那次安排了退避重试，这里只需把拒绝透传给调用方
    return inFlight.then(() => flushUserdataQueue())
  }
  if (pending.size === 0) return Promise.resolve()
  // 捕获每个文档发送时的版本；发给后端的载荷不带 version
  const sent = [...pending.entries()].map(([name, entry]) => ({
    name,
    value: entry.value,
    version: entry.version,
  }))
  inFlight = saveUserdataBatch(sent.map(({ name, value }) => ({ name, value }))).then(
    () => {
      // 只清掉发送后没被新写覆盖的文档（版本未变）；在途期间的新写留下一轮补发
      for (const doc of sent) {
        if (pending.get(doc.name)?.version === doc.version) pending.delete(doc.name)
      }
      retryAttempts = 0
      inFlight = null
      if (pending.size > 0) scheduleFlush(DEBOUNCE_MS)
    },
    (err: unknown) => {
      // 文档本就留在 pending 里（被覆盖的已是新值），安排退避重试
      inFlight = null
      scheduleRetry()
      throw err
    }
  )
  return inFlight
}

function scheduleRetry() {
  if (pending.size === 0) return
  if (retryAttempts >= RETRY_DELAYS_MS.length) return
  const delay = RETRY_DELAYS_MS[retryAttempts]
  retryAttempts += 1
  if (retryTimer) clearTimeout(retryTimer)
  retryTimer = setTimeout(() => {
    retryTimer = null
    void flushUserdataQueue().catch(() => undefined)
  }, delay)
}

/**
 * 登出/会话拆除时调用：丢弃全部待写文档并取消防抖与退避重试定时器。
 * 会话 Cookie 随登出失效，任何残留发送都只会 401 并触发 need-login，
 * 把刚登出的用户强制弹去登录页。各文档本地都有镜像（localStorage），
 * 下次登录的同步合并会推回，丢弃不丢数据。
 * 在途批量无法中断，但其落地回调对空队列全部 no-op（成功时版本守卫
 * 找不到条目、失败时 scheduleRetry 因 pending 为空直接返回）。
 */
export function cancelUserdataQueue() {
  if (debounceTimer) {
    clearTimeout(debounceTimer)
    debounceTimer = null
  }
  if (retryTimer) {
    clearTimeout(retryTimer)
    retryTimer = null
  }
  retryAttempts = 0
  pending.clear()
}

/**
 * 卸载/切后台时的最后一搏：发完即清，不等结果。这几类文档本地都有镜像
 * （localStorage），即使丢失也会在下次同步时补回；重复写是同值覆盖，无副作用。
 */
function flushWithKeepalive() {
  if (pending.size === 0) return
  const docs = [...pending.entries()].map(([name, entry]) => ({
    name,
    value: entry.value,
    version: entry.version,
  }))
  if (debounceTimer) {
    clearTimeout(debounceTimer)
    debounceTimer = null
  }
  if (retryTimer) {
    clearTimeout(retryTimer)
    retryTimer = null
  }
  try {
    void fetch(`${API_BASE}/saveUserdataBatch`, {
      method: 'POST',
      keepalive: true,
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ docs: docs.map(({ name, value }) => ({ name, value })) }),
    })
    // 直发绕过了 saveUserdataBatch 的封装，写后失效要在这里补齐：按发送的
    // 文档名同步失效 30s 预读缓存（只动本地缓存，不发额外请求），否则同会话
    // 随后读这些文档会消费到预读缓存里的旧值
    invalidateUserdataPrefetch(docs.map((doc) => doc.name))
    // fetch 成功发起才清已发送的版本（同步抛错如 fetch 不可用时保留待写）；
    // 读取后新写入的条目版本已变，不能一并清掉
    for (const doc of docs) {
      if (pending.get(doc.name)?.version === doc.version) pending.delete(doc.name)
    }
  } catch {
    // 卸载阶段无能为力
  }
}

if (typeof window !== 'undefined') {
  window.addEventListener('pagehide', flushWithKeepalive)
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'hidden') flushWithKeepalive()
  })
}
