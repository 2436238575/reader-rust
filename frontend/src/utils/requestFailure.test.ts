import { describe, expect, it } from 'vitest'
import { describeRequestFailure } from './requestFailure'

describe('describeRequestFailure', () => {
  it('浏览器离线时一律报离线，不看错误形态', () => {
    expect(describeRequestFailure(new Error('Network Error'), false)).toBe('离线中')
    const withStatus = Object.assign(new Error('无法连接上游站点'), { status: 502 })
    expect(describeRequestFailure(withStatus, false)).toBe('离线中')
    expect(describeRequestFailure(undefined, false)).toBe('离线中')
  })

  it('后端给了响应时用后端的真实原因（如 502 上游不可达）', () => {
    const err = Object.assign(new Error('无法连接上游站点'), { status: 502 })
    expect(describeRequestFailure(err, true)).toBe('无法连接上游站点')
  })

  it('401 的「需要登录」同样透传', () => {
    const err = Object.assign(new Error('需要登录'), { status: 401 })
    expect(describeRequestFailure(err, true)).toBe('需要登录')
  })

  it('在线但拿不到响应（服务器不可达/超时）时归因为服务器', () => {
    expect(describeRequestFailure(new Error('Network Error'), true)).toBe('无法连接服务器')
    expect(describeRequestFailure(new Error('timeout of 120000ms exceeded'), true)).toBe(
      '无法连接服务器'
    )
  })

  it('带了状态码但 message 为空时兜底为服务器不可达', () => {
    const err = Object.assign(new Error(''), { status: 500 })
    expect(describeRequestFailure(err, true)).toBe('无法连接服务器')
  })
})
