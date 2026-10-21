import { getUserdata, saveUserdata } from '../api/userdata'

/**
 * 常驻关键词高亮的配置存储。
 *
 * 走 userdata 通道（后端 `json_documents` 通用键值表），localStorage 是同步
 * 镜像：本地先写、防抖推后端，拉取时按 id 并集合并（冲突取 updatedAt 大者）。
 * 删除不刻墓碑——一端删除后另一端再同步会把它带回来，属可接受的近似
 * （与 readingStats 的合并语义一致）。
 */

export const KEYWORD_HIGHLIGHTS_DOC_NAME = 'keyword-highlights'
const LOCAL_STORAGE_KEY = 'reader-keyword-highlights'
const PUSH_DEBOUNCE_MS = 1500

export interface KeywordHighlightRule {
  id: string
  keyword: string
  /** 标记底色（CSS 颜色值，由管理界面的色盘给出） */
  color: string
  enabled: boolean
  /** 生效范围：全局 / 仅某本书 */
  scope: 'global' | 'book'
  /** scope=book 时的书籍 id */
  bookId?: string
  /** 双端合并用的最后更新时间（毫秒） */
  updatedAt: number
}

/** 管理界面的固定色盘：半透明底色，深浅色主题下都不至于糊掉正文。 */
export const KEYWORD_COLORS = [
  '#ffd54a80',
  '#7ee08a80',
  '#7cc4ff80',
  '#ff9fb280',
  '#ffb37c80',
  '#d3a6ff80',
]

export const DEFAULT_KEYWORD_COLOR = KEYWORD_COLORS[0]

/** 容错解析：任意来源（localStorage / 后端文档）的形状不对时丢掉坏条目 */
export function sanitizeKeywordRules(value: unknown): KeywordHighlightRule[] {
  if (!Array.isArray(value)) return []
  const rules: KeywordHighlightRule[] = []
  for (const raw of value) {
    if (!raw || typeof raw !== 'object') continue
    const item = raw as Partial<KeywordHighlightRule>
    if (typeof item.id !== 'string' || !item.id) continue
    if (typeof item.keyword !== 'string' || !item.keyword.trim()) continue
    rules.push({
      id: item.id,
      keyword: item.keyword,
      color: typeof item.color === 'string' && item.color ? item.color : DEFAULT_KEYWORD_COLOR,
      enabled: item.enabled !== false,
      scope: item.scope === 'book' ? 'book' : 'global',
      bookId: typeof item.bookId === 'string' && item.bookId ? item.bookId : undefined,
      updatedAt: typeof item.updatedAt === 'number' && item.updatedAt > 0 ? item.updatedAt : 0,
    })
  }
  return rules
}

/** 双端合并：按 id 并集，冲突取 updatedAt 较新的一版。 */
export function mergeKeywordRules(
  local: KeywordHighlightRule[],
  remote: KeywordHighlightRule[]
): KeywordHighlightRule[] {
  const byId = new Map<string, KeywordHighlightRule>()
  for (const rule of local) byId.set(rule.id, rule)
  for (const rule of remote) {
    const existing = byId.get(rule.id)
    if (!existing || rule.updatedAt >= existing.updatedAt) {
      byId.set(rule.id, rule)
    }
  }
  return [...byId.values()]
}

export function loadLocalKeywordRules(): KeywordHighlightRule[] {
  try {
    return sanitizeKeywordRules(JSON.parse(localStorage.getItem(LOCAL_STORAGE_KEY) || 'null'))
  } catch {
    return []
  }
}

export function persistLocalKeywordRules(rules: KeywordHighlightRule[]) {
  try {
    localStorage.setItem(LOCAL_STORAGE_KEY, JSON.stringify(rules))
  } catch {
    // 配额满：保持内存态，下次成功再写
  }
}

/** 拉后端文档与本地合并；合并结果与后端不一致时推回。失败时静默保持本地。 */
export async function syncKeywordRulesFromBackend(
  local: KeywordHighlightRule[]
): Promise<KeywordHighlightRule[]> {
  let remote: unknown
  try {
    remote = await getUserdata(KEYWORD_HIGHLIGHTS_DOC_NAME)
  } catch {
    return local // 离线或未登录
  }
  const remoteRules = sanitizeKeywordRules(remote)
  const merged = mergeKeywordRules(local, remoteRules)
  if (JSON.stringify(merged) !== JSON.stringify(remoteRules)) {
    await saveUserdata(KEYWORD_HIGHLIGHTS_DOC_NAME, merged).catch(() => undefined)
  }
  persistLocalKeywordRules(merged)
  return merged
}

let pushTimer: number | null = null

/** 本地先写镜像，防抖推后端（连续编辑合并为一次写入）。 */
export function persistAndPushKeywordRules(rules: KeywordHighlightRule[]) {
  persistLocalKeywordRules(rules)
  if (pushTimer) clearTimeout(pushTimer)
  pushTimer = window.setTimeout(() => {
    pushTimer = null
    void saveUserdata(KEYWORD_HIGHLIGHTS_DOC_NAME, rules).catch(() => undefined)
  }, PUSH_DEBOUNCE_MS)
}

/** 生成规则 id：时间戳 + 随机尾，避免双端同时新增撞号。 */
export function newKeywordRuleId(): string {
  return `kw-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`
}
