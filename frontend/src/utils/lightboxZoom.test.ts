import { describe, expect, it } from 'vitest'
import {
  clampLightboxScale,
  normalizeLightboxZoom,
  pinchScaleAt,
  zoomTranslateAt,
} from './lightboxZoom'

describe('clampLightboxScale', () => {
  it('夹在 1–5 之间', () => {
    expect(clampLightboxScale(0.5)).toBe(1)
    expect(clampLightboxScale(2)).toBe(2)
    expect(clampLightboxScale(99)).toBe(5)
  })
})

describe('zoomTranslateAt', () => {
  const center = { x: 500, y: 400 }

  it('光标下的图点在缩放前后不动', () => {
    const cursor = { x: 700, y: 300 }
    const translate = { x: 40, y: -20 }
    const scale = 2
    const nextScale = 3
    const next = zoomTranslateAt(cursor, center, translate, scale, nextScale)
    // 图点 p 两种算法应一致
    const before = {
      x: (cursor.x - center.x - translate.x) / scale,
      y: (cursor.y - center.y - translate.y) / scale,
    }
    const after = {
      x: (cursor.x - center.x - next.x) / nextScale,
      y: (cursor.y - center.y - next.y) / nextScale,
    }
    expect(after.x).toBeCloseTo(before.x, 10)
    expect(after.y).toBeCloseTo(before.y, 10)
  })

  it('光标在正中心时位移按比例放大', () => {
    // 光标 = 中心：translate' = translate * ratio（锚点公式的化简）
    const next = zoomTranslateAt(center, center, { x: 10, y: 10 }, 1, 2)
    expect(next.x).toBeCloseTo(20)
    expect(next.y).toBeCloseTo(20)
  })
})

describe('pinchScaleAt', () => {
  it('距离翻倍 = scale 翻倍（夹在上限内）', () => {
    expect(pinchScaleAt(1, 100, 200)).toBe(2)
    expect(pinchScaleAt(3, 100, 400)).toBe(5)
  })

  it('起手距离为 0 不炸（按 1 计）', () => {
    expect(pinchScaleAt(2, 0, 100)).toBe(5)
  })
})

describe('normalizeLightboxZoom', () => {
  it('scale 回到 1 以下时位移归零', () => {
    expect(normalizeLightboxZoom(0.9, { x: 50, y: 50 })).toEqual({
      scale: 1,
      translate: { x: 0, y: 0 },
    })
  })

  it('放大态原样保留', () => {
    const t = { x: 12, y: -8 }
    expect(normalizeLightboxZoom(2, t)).toEqual({ scale: 2, translate: t })
  })
})
