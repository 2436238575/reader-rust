// @vitest-environment jsdom
import { afterEach, describe, expect, it } from 'vitest'
import {
  applyWebdavLocalState,
  captureWebdavLocalState,
  WEBDAV_LOCAL_STATE_KEYS,
} from './webdavLocalState'

describe('webdav local state', () => {
  afterEach(() => {
    for (const key of WEBDAV_LOCAL_STATE_KEYS) {
      localStorage.removeItem(key)
    }
    localStorage.removeItem('reader-auth-hint')
  })

  it('capture 只采集白名单里存在的键', () => {
    localStorage.setItem('theme', 'dark')
    localStorage.setItem('reader-auth-hint', 'not-backed-up')
    expect(captureWebdavLocalState()).toEqual({ theme: 'dark' })
  })

  it('apply 只写入白名单键，备份里的任意键不会污染 localStorage', () => {
    applyWebdavLocalState({
      theme: 'dark',
      'reader-auth-hint': 'attacker-controlled',
      unknown: 'x',
    })
    expect(localStorage.getItem('theme')).toBe('dark')
    expect(localStorage.getItem('reader-auth-hint')).toBeNull()
    expect(localStorage.getItem('unknown')).toBeNull()
  })

  it('备份缺少的已知键会被清掉（显式空对象的整份覆盖语义）', () => {
    localStorage.setItem('theme', 'light')
    localStorage.setItem('readConfig', '{"fontSize":20}')
    applyWebdavLocalState({ theme: 'dark' })
    expect(localStorage.getItem('theme')).toBe('dark')
    expect(localStorage.getItem('readConfig')).toBeNull()
  })

  it('localState 缺失（旧备份/手写文档）时不改动现有键', () => {
    localStorage.setItem('theme', 'light')
    applyWebdavLocalState(null)
    applyWebdavLocalState(undefined)
    expect(localStorage.getItem('theme')).toBe('light')
  })
})
