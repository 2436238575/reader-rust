import { describe, expect, it } from 'vitest'
import type { Bookmark } from '../types'
import {
  bookmarkKindOf,
  chapterPosToProgress,
  progressToChapterPos,
  splitBookmarksByKind,
} from './bookmarks'

function bm(kind?: Bookmark['kind']): Bookmark {
  return { bookId: 'book_1', bookName: '书', bookAuthor: '作者', kind }
}

describe('bookmarkKindOf', () => {
  it('underline 归划线，缺省/其他值归章书签（旧数据无 kind 字段）', () => {
    expect(bookmarkKindOf(bm('underline'))).toBe('underline')
    expect(bookmarkKindOf(bm('bookmark'))).toBe('bookmark')
    expect(bookmarkKindOf(bm(undefined))).toBe('bookmark')
    expect(bookmarkKindOf(bm('weird' as Bookmark['kind']))).toBe('bookmark')
  })
})

describe('splitBookmarksByKind', () => {
  it('按类型分列，原顺序保留', () => {
    const items = [bm('bookmark'), bm('underline'), bm(undefined), bm('underline')]
    const { bookmarks, underlines } = splitBookmarksByKind(items)
    expect(bookmarks).toHaveLength(2)
    expect(underlines).toHaveLength(2)
    expect(bookmarks[0]).toBe(items[0])
    expect(bookmarks[1]).toBe(items[2])
  })

  it('空数组', () => {
    const { bookmarks, underlines } = splitBookmarksByKind([])
    expect(bookmarks).toEqual([])
    expect(underlines).toEqual([])
  })
})

describe('progressToChapterPos', () => {
  it('0–1 换算成万分比并取整', () => {
    expect(progressToChapterPos(0)).toBe(0)
    expect(progressToChapterPos(0.5)).toBe(5000)
    expect(progressToChapterPos(1)).toBe(10000)
    expect(progressToChapterPos(0.34567)).toBe(3457)
  })

  it('越界夹取', () => {
    expect(progressToChapterPos(-0.2)).toBe(0)
    expect(progressToChapterPos(1.5)).toBe(10000)
  })
})

describe('chapterPosToProgress', () => {
  it('万分比换算回 0–1', () => {
    expect(chapterPosToProgress(0)).toBe(0)
    expect(chapterPosToProgress(5000)).toBe(0.5)
    expect(chapterPosToProgress(10000)).toBe(1)
  })

  it('缺省与越界按章首/章末夹取（旧像素值不会再弹到章末之外）', () => {
    expect(chapterPosToProgress(undefined)).toBe(0)
    expect(chapterPosToProgress(-3)).toBe(0)
    expect(chapterPosToProgress(99999)).toBe(1)
  })
})
