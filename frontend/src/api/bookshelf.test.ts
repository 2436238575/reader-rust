// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from 'vitest'
import { getCoverUrl } from './bookshelf'

describe('getCoverUrl', () => {
  beforeEach(() => {
    localStorage.clear()
  })

  it('passes the backend image route through with the token', () => {
    localStorage.setItem('accessToken', 'tok')
    // 后端把书源地址改写成了本站取图地址：id 由后端映射表维护，前端原样使用
    const url = getCoverUrl('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3')
    expect(url).toBe('/reader3/image/9a30fa276d46103ce2fdc0ef47f508d3?accessToken=tok')
  })

  it('still routes raw source urls through the compat cover endpoint', () => {
    localStorage.setItem('accessToken', 'tok')
    // 历史书架记录里存的是书源地址（含签名），走兼容入口
    const url = getCoverUrl('https://img.example/a.heic?x-signature=abc')
    expect(url.startsWith('/reader3/cover?path=')).toBe(true)
    expect(url).toContain('accessToken=tok')
    expect(url).toContain(encodeURIComponent('https://img.example/a.heic?x-signature=abc'))
  })

  it('returns empty for missing cover', () => {
    expect(getCoverUrl(undefined)).toBe('')
    expect(getCoverUrl('')).toBe('')
  })
})
