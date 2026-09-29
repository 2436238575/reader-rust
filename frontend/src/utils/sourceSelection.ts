import type { BookSource } from '../types'

export function getVisibleSelection<T>(
  visibleItems: T[],
  selectedKeys: ReadonlySet<string>,
  keyOf: (item: T) => string
) {
  return visibleItems.filter((item) => selectedKeys.has(keyOf(item)))
}

export function toBookSourceDeletePayload(sources: Pick<BookSource, 'bookSourceUrl'>[]) {
  return sources.map((source) => ({ bookSourceUrl: source.bookSourceUrl }))
}

// 书源分组字段的拆分是全站唯一约定：中英文逗号/分号、顿号、竖线、斜杠都视为分隔符。
export function splitBookSourceGroups(group?: string | null): string[] {
  return (group || '')
    .split(/[,;，；、|/]/)
    .map((item) => item.trim())
    .filter(Boolean)
}

export function getBookSourceGroups(sources: Pick<BookSource, 'bookSourceGroup'>[]) {
  const groups = new Set<string>()
  sources.forEach((source) => {
    splitBookSourceGroups(source.bookSourceGroup).forEach((group) => groups.add(group))
  })
  return Array.from(groups).sort()
}

export function filterBookSources(sources: BookSource[], filterText: string, filterGroup: string) {
  const keyword = filterText.trim().toLowerCase()
  return sources.filter((source) => {
    const matchesText =
      !keyword ||
      source.bookSourceName.toLowerCase().includes(keyword) ||
      source.bookSourceUrl.toLowerCase().includes(keyword)
    // 分组过滤走拆分后的精确匹配（选项本就来自 getBookSourceGroups 的精确组名），
    // 避免子串匹配把「新发现」命中成「新」
    const matchesGroup =
      !filterGroup || splitBookSourceGroups(source.bookSourceGroup).includes(filterGroup)
    return matchesText && matchesGroup
  })
}

export function getBookSourceStats(allSources: BookSource[], filteredSources: BookSource[]) {
  return {
    total: allSources.length,
    enabled: allSources.filter((source) => source.enabled !== false).length,
    filtered: filteredSources.length,
  }
}

export function getBookSourceOverview(source: BookSource | null) {
  if (!source) {
    return null
  }
  return {
    group: source.bookSourceGroup?.trim() || '未分组',
    statusText: source.enabled === false ? '停用' : '启用',
    exploreText: source.enabledExplore === false ? '发现停用' : '发现可用',
    cookieText: source.enabledCookieJar ? 'Cookie 独立' : '默认 Cookie',
    hasSearch: Boolean(source.searchUrl || source.ruleSearch),
    hasExplore: Boolean(source.exploreUrl || source.ruleExplore),
    hasBookInfo: Boolean(source.ruleBookInfo),
    hasToc: Boolean(source.ruleToc),
    hasContent: Boolean(source.ruleContent),
    hasReview: Boolean(source.ruleReview || source.ruleParaReview),
    hasLogin: Boolean(source.loginUrl?.trim()),
  }
}
