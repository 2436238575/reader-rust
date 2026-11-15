import { API_V1_BASE } from './appBase'
import { reportReadEvents, type ReadEventPayload } from '../api/readStats'

/**
 * 阅读事件离线缓冲。
 *
 * 客户端按章累计「当日秒数」，每次 flush 上报累计值（服务端 upsert 取 MAX），
 * 因此成功上报后不能把计数清零；只有服务端返回的 `dayKey` 变化时才整体清零。
 * 离线/关页时缓冲落在 localStorage，恢复联网后继续 flush。
 */

const STORAGE_KEY = 'reader-read-events-v1'
const MAX_EVENTS_PER_BATCH = 100
const MAX_SECONDS_PER_CHAPTER_DAY = 86_400

interface BufferedEvent extends ReadEventPayload {
  /** 上次成功上报的累计秒数；只在时钟/本地存储里用，不上线缆 */
  lastSentSeconds?: number
}

interface StoredBuffer {
  /** 上一次服务端确认的自然日；null 表示还没校准过 */
  dayKey: number | null
  entries: BufferedEvent[]
}

let buffer: StoredBuffer = loadBuffer()
let inFlight: Promise<void> | null = null

function loadBuffer(): StoredBuffer {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return { dayKey: null, entries: [] }
    const parsed = JSON.parse(raw) as Partial<StoredBuffer>
    const entries = Array.isArray(parsed.entries)
      ? parsed.entries.filter(
          (entry): entry is BufferedEvent =>
            Boolean(entry) &&
            typeof entry.bookId === 'string' &&
            entry.bookId.length > 0 &&
            Number.isInteger(entry.chapterIndex) &&
            entry.chapterIndex >= 0 &&
            Number.isFinite(entry.seconds) &&
            entry.seconds >= 0
        )
      : []
    return {
      dayKey:
        typeof parsed.dayKey === 'number' && Number.isInteger(parsed.dayKey)
          ? parsed.dayKey
          : null,
      entries: entries.map((entry) => {
        const seconds = Math.min(
          Math.floor(entry.seconds),
          MAX_SECONDS_PER_CHAPTER_DAY
        )
        const lastSent =
          typeof entry.lastSentSeconds === 'number' &&
          Number.isFinite(entry.lastSentSeconds)
            ? Math.min(Math.max(0, Math.floor(entry.lastSentSeconds)), seconds)
            : undefined
        return {
          bookId: entry.bookId,
          chapterIndex: entry.chapterIndex,
          seconds,
          ...(lastSent != null ? { lastSentSeconds: lastSent } : {}),
        }
      }),
    }
  } catch {
    return { dayKey: null, entries: [] }
  }
}

function persist() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(buffer))
  } catch {
    /* 本地存储不可用时只保留内存缓冲 */
  }
}

function findEntry(bookId: string, chapterIndex: number) {
  return buffer.entries.find(
    (entry) => entry.bookId === bookId && entry.chapterIndex === chapterIndex
  )
}

/** 打开/切换到某章：先登记 0 秒，使「已读章数」不依赖是否读满 1 秒。 */
export function trackChapterRead(bookId: string, chapterIndex: number) {
  if (!bookId || !Number.isInteger(chapterIndex) || chapterIndex < 0) return
  if (findEntry(bookId, chapterIndex)) return
  buffer.entries.push({ bookId, chapterIndex, seconds: 0 })
  persist()
}

/** 累加该章当日秒数（封顶 24 小时）。 */
export function accumulateReadSeconds(
  bookId: string,
  chapterIndex: number,
  seconds: number
) {
  if (!bookId || !Number.isInteger(chapterIndex) || chapterIndex < 0) return
  if (!Number.isFinite(seconds) || seconds <= 0) return
  let entry = findEntry(bookId, chapterIndex)
  if (!entry) {
    entry = { bookId, chapterIndex, seconds: 0 }
    buffer.entries.push(entry)
  }
  entry.seconds = Math.min(
    MAX_SECONDS_PER_CHAPTER_DAY,
    entry.seconds + Math.floor(seconds)
  )
  persist()
}

export function pendingReadEvents(): ReadEventPayload[] {
  return buffer.entries.map(({ bookId, chapterIndex, seconds }) => ({
    bookId,
    chapterIndex,
    seconds,
  }))
}

/** 自上次成功上报后有变化的条目（0 秒新章也算变化）。 */
export function dirtyReadEvents(): ReadEventPayload[] {
  return buffer.entries
    .filter((entry) => {
      const sent = entry.lastSentSeconds ?? 0
      return entry.seconds > sent || (entry.seconds === 0 && entry.lastSentSeconds == null)
    })
    .map(({ bookId, chapterIndex, seconds }) => ({
      bookId,
      chapterIndex,
      seconds,
    }))
}

/** 测试/调试用：清空缓冲（不影响已上报数据）。 */
export function resetReadEventBuffer() {
  buffer = { dayKey: null, entries: [] }
  persist()
}

/**
 * 校准服务端自然日：空批次只取 `dayKey`。跨日后清空本地累计值，避免把
 * 昨天累计的秒数当成今天继续上报（服务端取 MAX 会把旧值带进新一天）。
 */
export async function syncReadDayKey(): Promise<number | null> {
  const result = await reportReadEvents([])
  applyServerDayKey(result.dayKey)
  return result.dayKey
}

function applyServerDayKey(dayKey: number) {
  if (buffer.dayKey != null && buffer.dayKey !== dayKey) {
    buffer = { dayKey, entries: [] }
  } else {
    buffer.dayKey = dayKey
  }
  persist()
}

function chunks<T>(items: T[], size: number): T[][] {
  const result: T[][] = []
  for (let index = 0; index < items.length; index += size) {
    result.push(items.slice(index, index + size))
  }
  return result
}

function eventKey(entry: ReadEventPayload) {
  return `${entry.bookId}::${entry.chapterIndex}`
}

/**
 * 上报当前缓冲（单飞）。成功后：同一天保留正秒数条目（下次继续上报累计值），
 * 0 秒条目已完成「已读章」登记可以丢掉；跨日则整体清空。
 */
export function flushReadEvents(): Promise<void> {
  if (inFlight) return inFlight
  const snapshot = dirtyReadEvents()
  if (!snapshot.length) return Promise.resolve()
  inFlight = (async () => {
    let serverDayKey = buffer.dayKey
    for (const chunk of chunks(snapshot, MAX_EVENTS_PER_BATCH)) {
      const dayKey = buffer.dayKey
      const result =
        dayKey == null
          ? await reportReadEvents(chunk)
          : await reportReadEvents(chunk, dayKey)
      serverDayKey = result.dayKey
    }
    if (serverDayKey == null) return
    if (buffer.dayKey != null && buffer.dayKey !== serverDayKey) {
      buffer = { dayKey: serverDayKey, entries: [] }
    } else {
      const sent = new Map(snapshot.map((entry) => [eventKey(entry), entry]))
      buffer = {
        dayKey: serverDayKey,
        entries: buffer.entries.filter((entry) => {
          const sentEntry = sent.get(eventKey(entry))
          if (!sentEntry) return true // flush 期间新增：保持待上报
          if (entry.seconds === 0) return false // 已读章已登记，不用每次重发
          // 只记「已发出去的累计值」；flush 期间又增长的部分下次继续发
          entry.lastSentSeconds = Math.min(entry.seconds, sentEntry.seconds)
          return true
        }),
      }
    }
    persist()
  })().finally(() => {
    inFlight = null
  })
  return inFlight
}

/**
 * pagehide/beforeunload 的兜底上报：不读响应、不改缓冲（请求可能被浏览器
 * 丢弃）；下次正常 flush 会按累计值重发，服务端 MAX 保证不翻倍。
 */
export function flushReadEventsKeepalive() {
  const snapshot = dirtyReadEvents()
  if (!snapshot.length) return
  for (const chunk of chunks(snapshot, MAX_EVENTS_PER_BATCH)) {
    void fetch(`${API_V1_BASE}/me/read-events:batch`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Requested-With': 'XMLHttpRequest',
      },
      credentials: 'same-origin',
      keepalive: true,
      body: JSON.stringify({ dayKey: buffer.dayKey, events: chunk }),
    }).catch(() => undefined)
  }
}

/** 测试用：直接查看缓存的 dayKey。 */
export function currentReadDayKey(): number | null {
  return buffer.dayKey
}
