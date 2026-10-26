import type { BookSource } from '../types'

export function getVisibleSelection<T>(
  visibleItems: T[],
  selectedKeys: ReadonlySet<string>,
  keyOf: (item: T) => string
) {
  return visibleItems.filter((item) => selectedKeys.has(keyOf(item)))
}

export function toBookSourceDeletePayload(sources: Pick<BookSource, 'bookSourceId'>[]) {
  return sources
    .map((source) => source.bookSourceId)
    .filter((id): id is number => id != null)
    .map((bookSourceId) => ({ bookSourceId }))
}

// 书源分组拆分以后端 split_source_groups（src/service/book_source_service.rs）为唯一权威：
// 只认半角逗号/分号、全角分号、顿号。前端不得比后端更宽（全角逗号、竖线、斜杠都不是分隔符），
// 否则「删除失效书源」等按分组判定的本地移除会与后端实际删除范围不一致。
export function splitBookSourceGroups(group?: string | null): string[] {
  return (group || '')
    .split(/[,;；、]/)
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
