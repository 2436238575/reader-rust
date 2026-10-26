import { describe, expect, it } from 'vitest'
import { fontPresets, themePresets } from './readerTheme'

describe('readerTheme 预设常量', () => {
  it('暗夜预设固定在末尾（夜间模式取数组最后一项）', () => {
    expect(themePresets.length).toBeGreaterThan(1)
    expect(themePresets[themePresets.length - 1].name).toBe('暗夜')
  })

  it('字体预设 value 唯一，系统字体为空 family', () => {
    const values = fontPresets.map((preset) => preset.value)
    expect(new Set(values).size).toBe(values.length)
    expect(fontPresets[0]).toMatchObject({ value: 'system', family: '' })
  })
})
