import http from './http'
import { API_V1_BASE } from '../utils/appBase'
import type { ReplaceRule } from '../types'

/**
 * 替换规则 CRUD（v1）。
 *
 * 身份是 `name`（存储主键 `(user_ns, name)`）；线缆上没有 `id` 字段，
 * 适配层补 `id: 0` 与旧形状对齐（Legado 导入携带的旧字段，恒 0）。
 */

/** v1 ReplaceRuleItem → 旧 ReplaceRule 形状 */
function mapReplaceRule(item: {
  name: string
  group?: string | null
  pattern: string
  replacement: string
  scope?: string | null
  isEnabled: boolean
  isRegex: boolean
  order: number
}): ReplaceRule {
  return {
    id: 0,
    name: item.name,
    group: item.group ?? undefined,
    pattern: item.pattern,
    replacement: item.replacement,
    scope: item.scope ?? undefined,
    isEnabled: item.isEnabled,
    isRegex: item.isRegex,
    order: item.order,
  }
}

/** 旧 ReplaceRule → v1 请求体（剥掉 `id`，v1 deny_unknown_fields 会拒绝） */
function toWire(rule: ReplaceRule) {
  return {
    name: rule.name,
    ...(rule.group != null ? { group: rule.group } : {}),
    pattern: rule.pattern,
    replacement: rule.replacement,
    ...(rule.scope != null ? { scope: rule.scope } : {}),
    isEnabled: rule.isEnabled,
    isRegex: rule.isRegex,
    order: rule.order,
  }
}

/** v1 ReplaceRuleItem 线缆形状（无 `id` 字段） */
interface ReplaceRuleItemWire {
  name: string
  group?: string | null
  pattern: string
  replacement: string
  scope?: string | null
  isEnabled: boolean
  isRegex: boolean
  order: number
}

/** 获取所有净化规则 */
export function getReplaceRules() {
  return http
    .get<ReplaceRuleItemWire[]>('/me/replace-rules', { baseURL: API_V1_BASE })
    .then((r) => r.data.map(mapReplaceRule))
}

/**
 * 保存单个净化规则（按 name upsert）
 */
export function saveReplaceRule(rule: ReplaceRule) {
  return http
    .post('/me/replace-rules', toWire(rule), { baseURL: API_V1_BASE })
    .then(() => '')
}

/** 批量操作的分片大小：服务端单批上限 100，按 100 串行分片 */
const REPLACE_RULES_CHUNK = 100

/**
 * 批量保存净化规则（按 100 分片串行，整本恢复不撞上限）
 */
export async function saveReplaceRules(rules: ReplaceRule[]) {
  for (let i = 0; i < rules.length; i += REPLACE_RULES_CHUNK) {
    await http.post(
      '/me/replace-rules:batch',
      { rules: rules.slice(i, i + REPLACE_RULES_CHUNK).map(toWire) },
      { baseURL: API_V1_BASE },
    )
  }
  return ''
}

/**
 * 删除单个净化规则（幂等）
 */
export function deleteReplaceRule(rule: ReplaceRule) {
  return http
    .delete(`/me/replace-rules/${encodeURIComponent(rule.name)}`, {
      baseURL: API_V1_BASE,
    })
    .then(() => '')
}

/**
 * 批量删除净化规则（幂等；不存在的名字忽略；按 100 分片串行）
 */
export async function deleteReplaceRules(rules: ReplaceRule[]) {
  for (let i = 0; i < rules.length; i += REPLACE_RULES_CHUNK) {
    await http.post(
      '/me/replace-rules:batchDelete',
      { names: rules.slice(i, i + REPLACE_RULES_CHUNK).map((rule) => rule.name) },
      { baseURL: API_V1_BASE },
    )
  }
  return ''
}
