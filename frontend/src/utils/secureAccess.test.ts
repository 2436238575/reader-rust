import { describe, expect, it } from 'vitest'
import { ACCESS_TOKEN_STORAGE_KEY, appendAuthQueryParams, readAccessToken } from './secureAccess'

function storageWith(entries: Record<string, string>) {
  return {
    getItem(key: string) {
      return entries[key] ?? null
    },
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
})
