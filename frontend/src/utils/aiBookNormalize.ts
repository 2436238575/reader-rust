/**
 * AI 书籍资料（人物/地点/关系/笔记）的归一化与合并原语。
 *
 * 生成管线（aiBookGeneration）、展示折叠（aiBookPresentation）与视图（AiBookView）
 * 共用这一份——此前三处各抄一份，已漂移风险最高的是 importance/低价值关系的判定口径。
 *
 * 注意：合并方向有意不成对——生成期合并偏好新数据（next 优先），
 * 展示期合并偏好先到数据（current 优先），所以 merge* 函数不在这里共享，
 * 只有无方向性的原语共享。
 */

/** 归一化键：忽略大小写/空白/中点变体，用于名称去重与相等判断。 */
export function normalizeKey(value: string | undefined) {
  return (value || '')
    .trim()
    .toLowerCase()
    .replace(/[·•・]/g, '.')
    .replace(/\s+/g, '')
}

export function isLowImportance(value: string | undefined) {
  const key = normalizeKey(value)
  if (!key) return false
  return [
    'low',
    '低',
    '低重要性',
    '不重要',
    '路人',
    '背景',
    'minor',
    'background',
    'oneoff',
    '一次性',
  ].some((term) => key.includes(term))
}

/** 0=未标注/未知 1=低 2=中 3=高 */
export function importanceRank(value: string | undefined) {
  const key = normalizeKey(value)
  if (key.includes('high') || key.includes('高')) return 3
  if (key.includes('medium') || key.includes('中')) return 2
  if (isLowImportance(value)) return 1
  return 0
}

/** 取内容更充实的那个（长者为优）。 */
export function richerString(current: string | undefined, next: string | undefined) {
  if (!current) return next || ''
  if (!next) return current
  return next.length > current.length ? next : current
}

export function preferImportance(current: string | undefined, next: string | undefined) {
  return importanceRank(next) > importanceRank(current) ? next : current || next
}

export function uniqueStrings(values: string[]) {
  const seen = new Set<string>()
  const result: string[] = []
  for (const value of values) {
    const key = normalizeKey(value)
    if (!key || seen.has(key)) continue
    seen.add(key)
    result.push(value)
  }
  return result
}

/** 关系去重键：无向点对 + 关系词。 */
export function relationshipKey(source: string, target: string, relation: string) {
  return `${[normalizeKey(source), normalizeKey(target)].sort().join('::')}::${normalizeKey(relation)}`
}

/** 「认识/见过/路过」这类弱关系且描述过短时视为低价值，过滤噪音边。 */
export function isLowValueRelationship(
  relation: string,
  detail: string,
  importance: string | undefined
) {
  const key = normalizeKey(importance)
  if (key.includes('high') || key.includes('medium') || key.includes('高') || key.includes('中')) {
    return false
  }
  if (!['认识', '见过', '路过', '同村', '同校', '位于', '相关'].includes(normalizeKey(relation))) {
    return false
  }
  return normalizeKey(detail).length < 18
}
