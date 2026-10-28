import { describe, expect, it } from 'vitest'
import { buildPageItems, clampPageOf, pageCountOf, pageRangeText } from './listPager'

describe('pageCountOf', () => {
  it('按条目数与每页大小换算总页数', () => {
    expect(pageCountOf(0, 50)).toBe(1)
    expect(pageCountOf(1, 50)).toBe(1)
    expect(pageCountOf(50, 50)).toBe(1)
    expect(pageCountOf(51, 50)).toBe(2)
    expect(pageCountOf(101, 50)).toBe(3)
  })

  it('非法输入按 1 页兜底', () => {
    expect(pageCountOf(100, 0)).toBe(100)
    expect(pageCountOf(100, -5)).toBe(100)
    expect(pageCountOf(-1, 50)).toBe(1)
    expect(pageCountOf(Number.NaN, 50)).toBe(1)
  })
})

describe('clampPageOf', () => {
  it('把页码收敛进合法范围', () => {
    expect(clampPageOf(3, 10)).toBe(3)
    expect(clampPageOf(0, 10)).toBe(1)
    expect(clampPageOf(-2, 10)).toBe(1)
    expect(clampPageOf(11, 10)).toBe(10)
    expect(clampPageOf(Number.NaN, 10)).toBe(1)
  })

  it('总页数至少为 1', () => {
    expect(clampPageOf(5, 0)).toBe(1)
  })
})

describe('pageRangeText', () => {
  it('输出当前页覆盖的区间与总数', () => {
    expect(pageRangeText(130, 50, 1)).toBe('1 - 50 共 130 个')
    expect(pageRangeText(130, 50, 2)).toBe('51 - 100 共 130 个')
    expect(pageRangeText(130, 50, 3)).toBe('101 - 130 共 130 个')
  })

  it('超界的页码按收敛后的位置计算', () => {
    expect(pageRangeText(130, 50, 9)).toBe('101 - 130 共 130 个')
  })

  it('空列表与非法 pageSize', () => {
    expect(pageRangeText(0, 50, 1)).toBe('共 0 个')
    expect(pageRangeText(-3, 50, 1)).toBe('共 0 个')
    expect(pageRangeText(130, 0, 1)).toBe('1 - 1 共 130 个')
  })
})

describe('buildPageItems', () => {
  it('页数不多时全部平铺', () => {
    expect(buildPageItems(1, 1)).toEqual([1])
    expect(buildPageItems(7, 4)).toEqual([1, 2, 3, 4, 5, 6, 7])
  })

  it('当前页在中间时首末常驻、两侧留省略号', () => {
    expect(buildPageItems(10, 5)).toEqual([1, 'ellipsis', 4, 5, 6, 'ellipsis', 10])
  })

  it('当前页贴近首页时左侧不出现省略号', () => {
    expect(buildPageItems(10, 1)).toEqual([1, 2, 'ellipsis', 10])
    expect(buildPageItems(10, 2)).toEqual([1, 2, 3, 'ellipsis', 10])
    expect(buildPageItems(10, 3)).toEqual([1, 2, 3, 4, 'ellipsis', 10])
  })

  it('当前页贴近末页时右侧不出现省略号', () => {
    expect(buildPageItems(10, 10)).toEqual([1, 'ellipsis', 9, 10])
    expect(buildPageItems(10, 9)).toEqual([1, 'ellipsis', 8, 9, 10])
  })

  it('超界的当前页按收敛后的位置计算窗口', () => {
    expect(buildPageItems(10, 99)).toEqual([1, 'ellipsis', 9, 10])
  })
})
