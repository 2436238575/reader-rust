import { describe, expect, it } from 'vitest'
import { formatChapterVersionMark } from './chapterVersionMark'
import type { ChapterVersionMark } from '../api/chapterEdit'

const DAY = new Date('2026-10-20T08:00:00').getTime()

function mark(partial: Partial<ChapterVersionMark>): ChapterVersionMark {
  return { kind: 'upstream', note: '', createdAt: DAY, isLatestUpstream: false, ...partial }
}

describe('formatChapterVersionMark', () => {
  it('没有版本记录时按最新上游处理', () => {
    expect(formatChapterVersionMark(undefined, '番茄Web')).toBe('[番茄Web] 最新')
  })

  it('最新上游标「最新」', () => {
    expect(formatChapterVersionMark(mark({ isLatestUpstream: true }), '番茄Web')).toBe(
      '[番茄Web] 最新'
    )
  })

  it('停在旧上游标版本日期', () => {
    expect(formatChapterVersionMark(mark({}), '番茄Web')).toBe('[番茄Web] 26-10-20')
  })

  it('编辑版默认标「编辑」，有备注用备注', () => {
    expect(formatChapterVersionMark(mark({ kind: 'edited' }), '番茄Web')).toBe('[编辑] 26-10-20')
    expect(
      formatChapterVersionMark(mark({ kind: 'edited', note: '润色错别字' }), '番茄Web')
    ).toBe('[润色错别字] 26-10-20')
  })

  it('导入版默认标「历史」，有备注用备注', () => {
    expect(formatChapterVersionMark(mark({ kind: 'imported' }), '番茄Web')).toBe(
      '[历史] 26-10-20'
    )
    expect(
      formatChapterVersionMark(mark({ kind: 'imported', note: '网盘版' }), '番茄Web')
    ).toBe('[网盘版] 26-10-20')
  })

  it('createdAt 为空时省略日期', () => {
    expect(formatChapterVersionMark(mark({ kind: 'edited', createdAt: 0 }), '本地')).toBe(
      '[编辑]'
    )
  })
})
