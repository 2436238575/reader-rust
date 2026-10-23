import { describe, expect, it } from 'vitest'
import {
  ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY,
  ACCESS_TOKEN_STORAGE_KEY,
  REFRESH_TOKEN_STORAGE_KEY,
  appendAuthQueryParams,
  clearAuthTokens,
  readAccessToken,
  readAccessTokenExpiresAt,
  readRefreshToken,
  storeAuthTokens,
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

describe('secureAccess', () => {
  it('reads the stored JWT verbatim', () => {
    const storage = storageWith({ [ACCESS_TOKEN_STORAGE_KEY]: ' header.payload.signature ' })
    expect(readAccessToken(storage)).toBe('header.payload.signature')
  })

  it('treats missing or blank tokens as absent', () => {
    expect(readAccessToken(storageWith({}))).toBeUndefined()
    expect(readAccessToken(storageWith({ [ACCESS_TOKEN_STORAGE_KEY]: '   ' }))).toBeUndefined()
  })

  it('adds the token to event source query params', () => {
    const params = new URLSearchParams()
    appendAuthQueryParams(params, storageWith({ [ACCESS_TOKEN_STORAGE_KEY]: 'alice-token' }))
    expect(params.get('accessToken')).toBe('alice-token')
  })

  it('omits the param entirely when there is no token', () => {
    const params = new URLSearchParams()
    appendAuthQueryParams(params, storageWith({}))
    expect(params.has('accessToken')).toBe(false)
  })

  it('stores and clears the access/refresh pair with an absolute expiry', () => {
    const storage = writableStorage()
    storeAuthTokens(
      { accessToken: 'access-a', refreshToken: 'rt_a', accessTokenExpiresIn: 3600 },
      storage
    )

    expect(readAccessToken(storage)).toBe('access-a')
    expect(readRefreshToken(storage)).toBe('rt_a')
    const expiresAt = readAccessTokenExpiresAt(storage)
    expect(expiresAt).toBeTypeOf('number')
    expect(expiresAt!).toBeGreaterThan(Date.now())

    clearAuthTokens(storage)
    expect(storage.getItem(ACCESS_TOKEN_STORAGE_KEY)).toBeNull()
    expect(storage.getItem(REFRESH_TOKEN_STORAGE_KEY)).toBeNull()
    expect(storage.getItem(ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY)).toBeNull()
  })

  it('ignores a corrupted expiry value', () => {
    const storage = storageWith({ [ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY]: 'not-a-number' })
    expect(readAccessTokenExpiresAt(storage)).toBeUndefined()
  })
})
