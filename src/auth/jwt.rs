use crate::error::error::{AppError, NEED_LOGIN};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// JWT 载荷。
///
/// `ver` 对应 `users.token_version`：改密码、重置密码、删除账号时自增，
/// 使此前签发的所有令牌立即失效。纯无状态令牌无法撤销——已删除的用户
/// 在过期前仍能访问——所以这里保留版本号，由中间件做一次服务端比对。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// 用户名（`users.username`）
    pub sub: String,
    /// 用户命名空间，直接参与 storage 路径拼接
    pub ns: String,
    pub is_admin: bool,
    /// 签发时间（Unix 秒）
    pub iat: i64,
    /// 过期时间（Unix 秒）
    pub exp: i64,
    /// 撤销版本号
    pub ver: i64,
}

/// 用 HS256 签发令牌。算法固定，不接受载荷指定。
pub fn encode_token(claims: &Claims, secret: &[u8]) -> Result<String, AppError> {
    encode(
        &Header::default(),
        claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("JWT 签发失败: {e}")))
}

/// 校验签名与 `exp`，失败一律映射为 401。
///
/// 刻意不把底层错误回传给客户端：区分「签名错误」「格式错误」「已过期」
/// 只会给攻击者提供额外信息。
pub fn decode_token(token: &str, secret: &[u8]) -> Result<Claims, AppError> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret),
        &Validation::default(),
    )
    .map(|data| data.claims)
    .map_err(|_| AppError::Unauthorized(NEED_LOGIN.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(exp: i64, ver: i64) -> Claims {
        Claims {
            sub: "reader1".to_string(),
            ns: "reader1".to_string(),
            is_admin: false,
            iat: 1_700_000_000,
            exp,
            ver,
        }
    }

    #[test]
    fn round_trips_all_claims() {
        let secret = b"test-secret";
        let token = encode_token(&claims(4_000_000_000, 3), secret).unwrap();
        let decoded = decode_token(&token, secret).unwrap();
        assert_eq!(decoded.sub, "reader1");
        assert_eq!(decoded.ns, "reader1");
        assert!(!decoded.is_admin);
        assert_eq!(decoded.ver, 3);
    }

    #[test]
    fn rejects_token_signed_with_another_secret() {
        let token = encode_token(&claims(4_000_000_000, 0), b"secret-a").unwrap();
        let err = decode_token(&token, b"secret-b").unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(message) if message == NEED_LOGIN));
    }

    #[test]
    fn rejects_tampered_payload() {
        let token = encode_token(&claims(4_000_000_000, 0), b"test-secret").unwrap();
        // 改动载荷后签名不再匹配
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        parts[1] = "eyJzdWIiOiJhdHRhY2tlciJ9".to_string();
        let tampered = parts.join(".");
        assert!(decode_token(&tampered, b"test-secret").is_err());
    }

    #[test]
    fn rejects_expired_token() {
        // 远早于当前时间，且超出 jsonwebtoken 默认的 60 秒容差
        let token = encode_token(&claims(1_000_000_000, 0), b"test-secret").unwrap();
        let err = decode_token(&token, b"test-secret").unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
    }

    #[test]
    fn rejects_garbage_input() {
        for bad in ["", "not-a-jwt", "a.b.c"] {
            assert!(decode_token(bad, b"test-secret").is_err());
        }
    }
}
