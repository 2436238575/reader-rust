import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import {
  deleteReplaceRule,
  deleteReplaceRules,
  getReplaceRules,
  saveReplaceRule,
  saveReplaceRules,
} from './replaceRule'
import { API_V1_BASE } from '../utils/appBase'
import type { ReplaceRule } from '../types'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
    delete: vi.fn(),
  },
}))

const getMock = http.get as unknown as Mock
const postMock = http.post as unknown as Mock
const deleteMock = http.delete as unknown as Mock

const rule: ReplaceRule = {
  id: 0,
  name: '去广告',
  group: '净化',
  pattern: '广告.*',
  replacement: '',
  scope: 'book:1',
  isEnabled: true,
  isRegex: true,
  order: 0,
}

describe('replaceRule（v1 接线）', () => {
  beforeEach(() => {
    getMock.mockReset()
    postMock.mockReset()
    deleteMock.mockReset()
  })

  it('getReplaceRules GET v1 路径并补 id:0', async () => {
    getMock.mockResolvedValue({
      data: [
        {
          name: '去广告',
          group: '净化',
          pattern: '广告.*',
          replacement: '',
          scope: 'book:1',
          isEnabled: true,
          isRegex: true,
          order: 0,
        },
      ],
    })

    const result = await getReplaceRules()

    expect(getMock).toHaveBeenCalledWith('/me/replace-rules', { baseURL: API_V1_BASE })
    expect(result[0]).toMatchObject({
      id: 0,
      name: '去广告',
      scope: 'book:1',
    })
  })

  it('saveReplaceRule POST 剥掉 id 字段（v1 deny_unknown_fields）', async () => {
    postMock.mockResolvedValue({ data: undefined })

    await saveReplaceRule(rule)

    expect(postMock).toHaveBeenCalledWith('/me/replace-rules', {
      name: '去广告',
      group: '净化',
      pattern: '广告.*',
      replacement: '',
      scope: 'book:1',
      isEnabled: true,
      isRegex: true,
      order: 0,
    }, { baseURL: API_V1_BASE })
  })

  it('saveReplaceRules 包成 {rules} 批量上送', async () => {
    postMock.mockResolvedValue({ data: { count: 1 } })

    await saveReplaceRules([rule])

    expect(postMock).toHaveBeenCalledWith(
      '/me/replace-rules:batch',
      { rules: [{ name: '去广告', group: '净化', pattern: '广告.*', replacement: '', scope: 'book:1', isEnabled: true, isRegex: true, order: 0 }] },
      { baseURL: API_V1_BASE },
    )
  })

  it('deleteReplaceRule DELETE 按 name 路径', async () => {
    deleteMock.mockResolvedValue({ data: undefined })

    await deleteReplaceRule(rule)

    expect(deleteMock).toHaveBeenCalledWith(
      `/me/replace-rules/${encodeURIComponent('去广告')}`,
      { baseURL: API_V1_BASE },
    )
  })

  it('deleteReplaceRules 批量走 names 字段', async () => {
    postMock.mockResolvedValue({ data: { deleted: 2 } })

    await deleteReplaceRules([rule, { ...rule, name: '第二条' }])

    expect(postMock).toHaveBeenCalledWith(
      '/me/replace-rules:batchDelete',
      { names: ['去广告', '第二条'] },
      { baseURL: API_V1_BASE },
    )
  })
})
