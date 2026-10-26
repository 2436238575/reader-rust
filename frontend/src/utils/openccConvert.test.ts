import { beforeEach, describe, expect, it, vi } from 'vitest'

// 模块级转换器缓存：每个用例用全新的模块实例，避免串状态
vi.mock('opencc-js/cn2t', () => ({
  Converter: vi.fn(() => (text: string) => `<tw>${text}</tw>`),
}))
vi.mock('opencc-js/t2cn', () => ({
  Converter: vi.fn(() => (text: string) => `<cn>${text}</cn>`),
}))

describe('openccConvert 简繁转换', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.clearAllMocks()
  })

  it('转换器未加载时原样返回（先显示原文）', async () => {
    const { convertChineseText } = await import('./openccConvert')
    expect(convertChineseText('正文', 'traditional')).toBe('正文')
    expect(convertChineseText('正文', 'simplified')).toBe('正文')
    expect(convertChineseText('', 'traditional')).toBe('')
  })

  it('traditional 模式按需加载 cn2t 并转换；重复加载共享同一 Promise', async () => {
    const { ensureChineseConverter, convertChineseText } = await import('./openccConvert')
    const { Converter } = await import('opencc-js/cn2t')

    await Promise.all([ensureChineseConverter('traditional'), ensureChineseConverter('traditional')])
    expect(Converter).toHaveBeenCalledTimes(1)
    expect(Converter).toHaveBeenCalledWith({ from: 'cn', to: 'tw' })
    expect(convertChineseText('正文', 'traditional')).toBe('<tw>正文</tw>')
    // simplified 方向的转换不受影响
    expect(convertChineseText('正文', 'simplified')).toBe('正文')
  })

  it('simplified 模式按需加载 t2cn 并转换', async () => {
    const { ensureChineseConverter, convertChineseText } = await import('./openccConvert')
    const { Converter } = await import('opencc-js/t2cn')

    await ensureChineseConverter('simplified')
    expect(Converter).toHaveBeenCalledWith({ from: 'tw', to: 'cn' })
    expect(convertChineseText('正文', 'simplified')).toBe('<cn>正文</cn>')
  })

  it('字典加载失败时保持原文返回', async () => {
    const { Converter } = await import('opencc-js/cn2t')
    vi.mocked(Converter).mockImplementationOnce(() => {
      throw new Error('字典损坏')
    })
    const { ensureChineseConverter, convertChineseText } = await import('./openccConvert')

    await ensureChineseConverter('traditional')
    expect(convertChineseText('正文', 'traditional')).toBe('正文')
  })
})
