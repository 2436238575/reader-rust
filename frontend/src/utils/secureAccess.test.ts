import { describe, expect, it } from 'vitest'
import {
  AUTH_HINT_STORAGE_KEY,
  LEGACY_ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY,
  LEGACY_ACCESS_TOKEN_STORAGE_KEY,
  LEGACY_REFRESH_TOKEN_STORAGE_KEY,
  clearAuthHint,
  clearLegacyAuthTokens,
  hasAuthHint,
  readAccessTokenExpiresAt,
  readAuthHint,
  readLegacyRefreshToken,
  storeAuthHint,
} from './secureAccess'

function storageWith(entries: Record<string, string>) {
  return {
    getItem(key: string) {
      return entries[key] ?? null
    },
  }
}

function writableStorage() {
  const entries = new Map<string, string>()
  return {
    getItem: (key: string) => entries.get(key) ?? null,
    setItem: (key: string, value: string) => entries.set(key, value),
    removeItem: (key: string) => entries.delete(key),
  }
}

describe('secureAccess 登录提示', () => {
  it('hint 读写往返：用户名与过期时间都保留', () => {
    const storage = writableStorage()
    const expiresAt = Date.now() + 3600 * 1000
    storeAuthHint({ username: 'alice', accessTokenExpiresAt: expiresAt }, storage)

    expect(hasAuthHint(storage)).toBe(true)
    expect(readAuthHint(storage)).toEqual({ username: 'alice', accessTokenExpiresAt: expiresAt })
    expect(readAccessTokenExpiresAt(storage)).toBe(expiresAt)

    clearAuthHint(storage)
    expect(hasAuthHint(storage)).toBe(false)
    expect(readAuthHint(storage)).toBeUndefined()
    expect(storage.getItem(AUTH_HINT_STORAGE_KEY)).toBeNull()
  })

  it('过期时间缺省的 hint 也算有效（未知到期时间不等于未登录）', () => {
    const storage = writableStorage()
    storeAuthHint({ username: 'alice' }, storage)

    expect(hasAuthHint(storage)).toBe(true)
    expect(readAuthHint(storage)).toEqual({ username: 'alice', accessTokenExpiresAt: undefined })
    expect(readAccessTokenExpiresAt(storage)).toBeUndefined()
  })

  it('内容损坏（非法 JSON / 非对象 / 非法过期值）按没有提示处理', () => {
    expect(readAuthHint(storageWith({ [AUTH_HINT_STORAGE_KEY]: '{not-json' }))).toBeUndefined()
    expect(readAuthHint(storageWith({ [AUTH_HINT_STORAGE_KEY]: '"a-string"' }))).toBeUndefined()
    expect(
      readAccessTokenExpiresAt(
        storageWith({ [AUTH_HINT_STORAGE_KEY]: JSON.stringify({ username: 'a', accessTokenExpiresAt: 'soon' }) })
      )
    ).toBeUndefined()
    expect(hasAuthHint(storageWith({ [AUTH_HINT_STORAGE_KEY]: '{not-json' }))).toBe(false)
  })

  it('空存储下所有读取都返回空值（无令牌可读）', () => {
    const storage = storageWith({})
    expect(readAuthHint(storage)).toBeUndefined()
    expect(hasAuthHint(storage)).toBe(false)
    expect(readAccessTokenExpiresAt(storage)).toBeUndefined()
    expect(readLegacyRefreshToken(storage)).toBeUndefined()
  })
})

describe('secureAccess 旧版令牌迁移', () => {
  it('readLegacyRefreshToken 读取旧键，空白视为不存在', () => {
    expect(readLegacyRefreshToken(storageWith({ [LEGACY_REFRESH_TOKEN_STORAGE_KEY]: ' rt_old ' }))).toBe(
      'rt_old'
    )
    expect(readLegacyRefreshToken(storageWith({ [LEGACY_REFRESH_TOKEN_STORAGE_KEY]: '   ' }))).toBeUndefined()
  })

  it('clearLegacyAuthTokens 删除三个旧键，不动 hint', () => {
    const storage = writableStorage()
    storage.setItem(LEGACY_ACCESS_TOKEN_STORAGE_KEY, 'old-access')
    storage.setItem(LEGACY_REFRESH_TOKEN_STORAGE_KEY, 'rt_old')
    storage.setItem(LEGACY_ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY, '123')
    storeAuthHint({ username: 'alice' }, storage)

    clearLegacyAuthTokens(storage)

    expect(storage.getItem(LEGACY_ACCESS_TOKEN_STORAGE_KEY)).toBeNull()
    expect(storage.getItem(LEGACY_REFRESH_TOKEN_STORAGE_KEY)).toBeNull()
    expect(storage.getItem(LEGACY_ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY)).toBeNull()
    expect(hasAuthHint(storage)).toBe(true)
  })
})
