import { describe, expect, it } from 'vitest'
import {
  importanceRank,
  isLowImportance,
  isLowValueRelationship,
  normalizeKey,
  preferImportance,
  relationshipKey,
  richerString,
  uniqueStrings,
} from './aiBookNormalize'

describe('aiBookNormalize', () => {
  it('normalizeKey 忽略大小写/空白/中点变体', () => {
    expect(normalizeKey(' 张三 ·Feng ')).toBe('张三.feng')
    expect(normalizeKey(undefined)).toBe('')
    expect(normalizeKey('')).toBe('')
  })

  it('isLowImportance 命中中英文低价值词', () => {
    expect(isLowImportance('路人')).toBe(true)
    expect(isLowImportance('Low')).toBe(true)
    expect(isLowImportance('重要角色')).toBe(false)
    expect(isLowImportance(undefined)).toBe(false)
  })

  it('importanceRank 四档', () => {
    expect(importanceRank('高')).toBe(3)
    expect(importanceRank('medium')).toBe(2)
    expect(importanceRank('背景')).toBe(1)
    expect(importanceRank(undefined)).toBe(0)
  })

  it('richerString 取内容更充实者', () => {
    expect(richerString(undefined, 'a')).toBe('a')
    expect(richerString('长描述', '短')).toBe('长描述')
    expect(richerString('短', '更长的描述')).toBe('更长的描述')
  })

  it('preferImportance 取评级更高者', () => {
    expect(preferImportance('低', '高')).toBe('高')
    expect(preferImportance('高', '低')).toBe('高')
  })

  it('uniqueStrings 按归一化键去重但保留首个原文', () => {
    expect(uniqueStrings(['张三', ' 张三 ', '李四', ''])).toEqual(['张三', '李四'])
  })

  it('relationshipKey 无向点对 + 关系词', () => {
    expect(relationshipKey('甲', '乙', '朋友')).toBe(relationshipKey('乙', '甲', '朋友'))
    expect(relationshipKey('甲', '乙', '朋友')).not.toBe(relationshipKey('甲', '乙', '敌人'))
  })

  it('isLowValueRelationship 只过滤弱关系且描述过短的边', () => {
    expect(isLowValueRelationship('认识', '见过一面', undefined)).toBe(true)
    expect(isLowValueRelationship('认识', '见过一面', '高')).toBe(false)
    expect(isLowValueRelationship('挚友', 'x', undefined)).toBe(false)
    expect(
      isLowValueRelationship('认识', '从第三章起共同行动并多次互相救援，结下深厚情谊', undefined)
    ).toBe(false)
  })
})
