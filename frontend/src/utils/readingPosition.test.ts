import { describe, expect, it } from 'vitest'
import {
  buildServerSavedPosition,
  getPositionStorageKey,
  normalizePositionTimestamp,
} from './readingPosition'

describe('readingPosition', () => {
  it('builds storage key from bookId only', () => {
    expect(getPositionStorageKey('book_42')).toBe('reader-position:book_42')
    expect(getPositionStorageKey('')).toBe('')
    expect(getPositionStorageKey(null)).toBe('')
    expect(getPositionStorageKey(undefined)).toBe('')
  })

  it('normalizes second/millisecond timestamps to milliseconds', () => {
    expect(normalizePositionTimestamp(1_700_000_000)).toBe(1_700_000_000_000)
    expect(normalizePositionTimestamp(1_700_000_000_000)).toBe(1_700_000_000_000)
    expect(normalizePositionTimestamp(0)).toBe(0)
    expect(normalizePositionTimestamp(-5)).toBe(0)
    expect(normalizePositionTimestamp(Number.NaN)).toBe(0)
    expect(normalizePositionTimestamp(null)).toBe(0)
    expect(normalizePositionTimestamp(undefined)).toBe(0)
  })

  it('builds server position from book progress fields', () => {
    const pos = buildServerSavedPosition(
      { durChapterIndex: 3, durChapterPos: 5000, durChapterTime: 1_700_000_000 },
      3,
      null
    )
    expect(pos).toEqual({
      chapterIndex: 3,
      progress: 0.5,
      updatedAt: 1_700_000_000_000,
    })
  })

  it('returns null when the book is on a different chapter or missing', () => {
    expect(buildServerSavedPosition(null, 3, null)).toBeNull()
    expect(buildServerSavedPosition({ durChapterIndex: 2, durChapterPos: 0 }, 3, null)).toBeNull()
  })

  it('clamps progress and handles already-normalized 0-1 values', () => {
    expect(
      buildServerSavedPosition({ durChapterIndex: 0, durChapterPos: 12000 }, 0, null)?.progress
    ).toBe(1)
    expect(
      buildServerSavedPosition({ durChapterIndex: 0, durChapterPos: 0.3 }, 0, null)?.progress
    ).toBe(0.3)
  })

  it('prefers serverProgressTime over durChapterTime for freshness comparison', () => {
    // durChapterTime 在打开章节时被刷成本机当前时间，直接用它会让服务端存档
    // 在本地/服务端比较里永远「更新」——必须优先用打开书时捕获的 serverProgressTime
    const pos = buildServerSavedPosition(
      { durChapterIndex: 1, durChapterPos: 0, durChapterTime: 9_999_999_999_999 },
      1,
      1_700_000_000 // 秒级服务端时间
    )
    expect(pos?.updatedAt).toBe(1_700_000_000_000)
  })
})
