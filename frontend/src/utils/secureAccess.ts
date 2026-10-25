// 会话凭据的前端存储。
//
// Access/Refresh Token 已全部改由 HttpOnly Cookie 承载，JS 读不到也存不了；
// 这里只维护一份非敏感的登录提示（hint）：用户名 + Access Token 过期时间，
// 供离线 UI、路由守卫与「到期前主动续期」使用，不含任何凭据。
//
// 旧版留在 localStorage 的三个令牌键只用于一次性迁移：把旧 Refresh Token
// 放进 /refreshToken 的 body 换取 Cookie 会话，迁移后即删除（见
// utils/authSession.ts 与 stores/app.ts 的 bootstrapAuth）。

/** 登录提示键：`{username, accessTokenExpiresAt}`，不含令牌。 */
export const AUTH_HINT_STORAGE_KEY = 'reader-auth-hint'

/** 旧版 Access Token 键：只读不写，迁移后删除。 */
export const LEGACY_ACCESS_TOKEN_STORAGE_KEY = 'accessToken'
/** 旧版 Refresh Token 键：迁移时读一次放进刷新 body，之后删除。 */
export const LEGACY_REFRESH_TOKEN_STORAGE_KEY = 'refreshToken'
/** 旧版过期时间键：只用于迁移后清理。 */
export const LEGACY_ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY = 'accessTokenExpiresAt'

type StorageLike = Pick<Storage, 'getItem'>
type WritableStorageLike = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>

/** 非敏感登录提示：用户名与 Access Token 的绝对过期时间。 */
export interface AuthHint {
  username: string
  /** Access Token 过期时间（毫秒）；未知时缺省。 */
  accessTokenExpiresAt?: number
}

function parseHint(raw: string | null): AuthHint | undefined {
  if (!raw) return undefined
  try {
    const parsed: unknown = JSON.parse(raw)
    if (typeof parsed !== 'object' || parsed === null) return undefined
    const record = parsed as Partial<AuthHint>
    const expiresAt =
      typeof record.accessTokenExpiresAt === 'number' &&
      Number.isFinite(record.accessTokenExpiresAt) &&
      record.accessTokenExpiresAt > 0
        ? record.accessTokenExpiresAt
        : undefined
    return {
      username: typeof record.username === 'string' ? record.username : '',
      accessTokenExpiresAt: expiresAt,
    }
  } catch {
    // 内容损坏（手改/写一半崩溃）：按没有提示处理
    return undefined
  }
}

/** 读取登录提示；不存在或内容损坏时返回 undefined。 */
export function readAuthHint(storage: StorageLike = localStorage): AuthHint | undefined {
  return parseHint(storage.getItem(AUTH_HINT_STORAGE_KEY))
}

/** 登录 / 刷新 / 改密成功后保存提示（覆盖式）。 */
export function storeAuthHint(hint: AuthHint, storage: WritableStorageLike = localStorage) {
  storage.setItem(AUTH_HINT_STORAGE_KEY, JSON.stringify(hint))
}

/** 登出或会话失效时清掉提示。 */
export function clearAuthHint(storage: WritableStorageLike = localStorage) {
  storage.removeItem(AUTH_HINT_STORAGE_KEY)
}

/**
 * 是否存在登录提示。
 *
 * 提示只说明「这台设备最近登录过」，不代表会话仍有效；
 * 真正的失效由 API 401 与刷新失败兜底。
 */
export function hasAuthHint(storage: StorageLike = localStorage): boolean {
  return readAuthHint(storage) !== undefined
}

/** Access Token 的绝对过期时间（毫秒），从 hint 读取；缺失或损坏时返回 undefined。 */
export function readAccessTokenExpiresAt(
  storage: StorageLike = localStorage
): number | undefined {
  return readAuthHint(storage)?.accessTokenExpiresAt
}

/** 读取旧版 localStorage Refresh Token；仅在迁移（换 Cookie 会话）时使用。 */
export function readLegacyRefreshToken(storage: StorageLike = localStorage): string | undefined {
  return storage.getItem(LEGACY_REFRESH_TOKEN_STORAGE_KEY)?.trim() || undefined
}

/** 删除旧版 localStorage 的三个令牌键；迁移完成（无论成败）后调用。 */
export function clearLegacyAuthTokens(storage: WritableStorageLike = localStorage) {
  storage.removeItem(LEGACY_ACCESS_TOKEN_STORAGE_KEY)
  storage.removeItem(LEGACY_REFRESH_TOKEN_STORAGE_KEY)
  storage.removeItem(LEGACY_ACCESS_TOKEN_EXPIRES_AT_STORAGE_KEY)
}
