import { describe, expect, it } from 'vitest'
import { isReaderInteractiveClickTarget, resolveReaderTapZone } from './readerClick'

describe('readerClick', () => {
  it('treats reader controls as interactive so clicks do not trigger page turning', () => {
    const target = fakeTarget((selector) => selector.includes('button'))

    expect(isReaderInteractiveClickTarget(target)).toBe(true)
  })

  it('treats mobile controls as interactive so touch pagination does not steal toolbar taps', () => {
    const target = fakeTarget((selector) => selector.includes('.mobile-controls'))

    expect(isReaderInteractiveClickTarget(target)).toBe(true)
  })

  it('treats plain chapter text as non-interactive', () => {
    const target = fakeTarget(() => false)

    expect(isReaderInteractiveClickTarget(target)).toBe(false)
  })
})

describe('resolveReaderTapZone', () => {
  // 300×900 网格：每格 100×300，四边留白 12px/36px
  const W = 300
  const H = 900

  it('左右翻页模式按九宫格映射', () => {
    const horizontal = true
    // 上一页 上一页 下一页
    expect(resolveReaderTapZone(50, 100, horizontal, W, H)).toBe('prev')
    expect(resolveReaderTapZone(150, 100, horizontal, W, H)).toBe('prev')
    expect(resolveReaderTapZone(250, 100, horizontal, W, H)).toBe('next')
    // 上一页 菜单 下一页
    expect(resolveReaderTapZone(50, 450, horizontal, W, H)).toBe('prev')
    expect(resolveReaderTapZone(150, 450, horizontal, W, H)).toBe('menu')
    expect(resolveReaderTapZone(250, 450, horizontal, W, H)).toBe('next')
    // 上一页 下一页 下一页
    expect(resolveReaderTapZone(50, 800, horizontal, W, H)).toBe('prev')
    expect(resolveReaderTapZone(150, 800, horizontal, W, H)).toBe('next')
    expect(resolveReaderTapZone(250, 800, horizontal, W, H)).toBe('next')
  })

  it('其他翻页模式按九宫格映射', () => {
    const horizontal = false
    // 上一页 上一页 上一页
    expect(resolveReaderTapZone(50, 100, horizontal, W, H)).toBe('prev')
    expect(resolveReaderTapZone(150, 100, horizontal, W, H)).toBe('prev')
    expect(resolveReaderTapZone(250, 100, horizontal, W, H)).toBe('prev')
    // 上一页 菜单 下一页
    expect(resolveReaderTapZone(50, 450, horizontal, W, H)).toBe('prev')
    expect(resolveReaderTapZone(150, 450, horizontal, W, H)).toBe('menu')
    expect(resolveReaderTapZone(250, 450, horizontal, W, H)).toBe('next')
    // 下一页 下一页 下一页
    expect(resolveReaderTapZone(50, 800, horizontal, W, H)).toBe('next')
    expect(resolveReaderTapZone(150, 800, horizontal, W, H)).toBe('next')
    expect(resolveReaderTapZone(250, 800, horizontal, W, H)).toBe('next')
  })

  it('四边留白带内的点按不触发动作', () => {
    for (const horizontal of [true, false]) {
      // 左缘 1%、右缘 99%、上缘 1%、下缘 99%
      expect(resolveReaderTapZone(W * 0.01, 450, horizontal, W, H)).toBeNull()
      expect(resolveReaderTapZone(W * 0.99, 450, horizontal, W, H)).toBeNull()
      expect(resolveReaderTapZone(150, H * 0.01, horizontal, W, H)).toBeNull()
      expect(resolveReaderTapZone(150, H * 0.99, horizontal, W, H)).toBeNull()
    }
  })

  it('非法尺寸返回 null', () => {
    expect(resolveReaderTapZone(150, 450, true, 0, 900)).toBeNull()
    expect(resolveReaderTapZone(150, 450, true, 300, 0)).toBeNull()
  })
})

function fakeTarget(matches: (selector: string) => boolean) {
  return {
    closest: (selector: string) => (matches(selector) ? {} : null),
  }
}
