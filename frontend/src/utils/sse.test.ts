import { describe, expect, it } from 'vitest'
import { parseSseErrorMsg, parseSseJson } from './sse'

describe('parseSseErrorMsg', () => {
  it('提取后端 error 帧的 errorMsg', () => {
    expect(parseSseErrorMsg('{"errorMsg":"未配置书源"}')).toBe('未配置书源')
  })

  it('空值/坏 JSON/空文案都返回 null', () => {
    expect(parseSseErrorMsg(undefined)).toBeNull()
    expect(parseSseErrorMsg('')).toBeNull()
    expect(parseSseErrorMsg('not json')).toBeNull()
    expect(parseSseErrorMsg('{"errorMsg":"  "}'))
      .toBeNull()
    expect(parseSseErrorMsg('{"data":[]}')).toBeNull()
  })
})

describe('parseSseJson', () => {
  it('解析 JSON 帧', () => {
    expect(parseSseJson<{ a: number }>('{"a":1}')).toEqual({ a: 1 })
  })

  it('空帧与坏帧返回 null', () => {
    expect(parseSseJson(null)).toBeNull()
    expect(parseSseJson('')).toBeNull()
    expect(parseSseJson('{')).toBeNull()
  })
})
