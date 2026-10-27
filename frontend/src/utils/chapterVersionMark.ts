import { formatShortDate } from './format'
import type { ChapterVersionMark } from '../api/chapterEdit'

/**
 * 目录章节行的版本标签：`[来源] 最新|YY-MM-DD`。
 *
 * - 上游：`[书源名] 最新`（最新上游）或 `[书源名] YY-MM-DD`（停在旧上游）
 * - 导入：`[备注或「历史」] YY-MM-DD`
 * - 编辑：`[备注或「编辑」] YY-MM-DD`
 * - 没有任何版本记录（未读过）：视为「最新上游」
 */
export function formatChapterVersionMark(
  mark: ChapterVersionMark | undefined,
  sourceName: string
): string {
  if (!mark) return `[${sourceName}] 最新`
  if (mark.kind === 'upstream') {
    return mark.isLatestUpstream
      ? `[${sourceName}] 最新`
      : `[${sourceName}] ${formatShortDate(mark.createdAt)}`
  }
  const label = mark.note || (mark.kind === 'imported' ? '历史' : '编辑')
  const date = formatShortDate(mark.createdAt)
  return date ? `[${label}] ${date}` : `[${label}]`
}
