use crate::error::error::{AppError, NEED_LOGIN};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// JWT 载荷。
///
/// `ver` 对应 `users.token_version`：改密码时自增，使此前签发的所有令牌
/// 立即失效。纯无状态令牌无法撤销——改密后旧令牌在过期前仍能访问——所以
/// 这里保留版本号，由中间件做一次服务端比对。
/// 限定用途令牌：书源登录代理。
///
/// 被代理页面是第三方书源的 HTML/JS，与主站同源吐出——把主 JWT 嵌进页面
/// 等于把账号交给书源。代理页面改发这种短寿命、绑定单个书源、且只能
/// 访问代理路径的令牌；泄漏了也只能用来继续代理同一书源的页面。
pub const PROXY_TOKEN_SCOPE: &str = "bookSourceProxy";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// 用户名（`users.username`）
    pub sub: String,
    /// 用户命名空间，直接参与 storage 路径拼接
    pub ns: String,
    /// 签发时间（Unix 秒）
    pub iat: i64,
    /// 过期时间（Unix 秒）
    pub exp: i64,
    /// 撤销版本号
    pub ver: i64,
    /// 限定用途（空 = 全功能主令牌）；目前是 [`PROXY_TOKEN_SCOPE`]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// 代理令牌绑定的书源（`bookSourceUrl`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bsu: Option<String>,
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
            iat: 1_700_000_000,
            exp,
            ver,
            scope: None,
            bsu: None,
        }
    }

    #[test]
    fn round_trips_all_claims() {
        let secret = b"test-secret";
        let token = encode_token(&claims(4_000_000_000, 3), secret).unwrap();
        let decoded = decode_token(&token, secret).unwrap();
        assert_eq!(decoded.sub, "reader1");
        assert_eq!(decoded.ns, "reader1");
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
    fn scoped_claims_round_trip() {
        let secret = b"test-secret";
        let mut scoped = claims(4_000_000_000, 1);
        scoped.scope = Some(PROXY_TOKEN_SCOPE.to_string());
        scoped.bsu = Some("https://source.example".to_string());
        let token = encode_token(&scoped, secret).unwrap();
        let decoded = decode_token(&token, secret).unwrap();
        assert_eq!(decoded.scope.as_deref(), Some(PROXY_TOKEN_SCOPE));
        assert_eq!(decoded.bsu.as_deref(), Some("https://source.example"));
        // 主令牌不带 scope 字段，序列化里不该出现（老令牌格式保持干净）
        let plain = encode_token(&claims(4_000_000_000, 1), secret).unwrap();
        let payload = plain.split('.').nth(1).unwrap().to_string();
        let json = String::from_utf8(
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, payload)
                .unwrap(),
        )
        .unwrap();
        assert!(!json.contains("scope") && !json.contains("bsu"));
    }

    #[test]
    fn rejects_garbage_input() {
        for bad in ["", "not-a-jwt", "a.b.c"] {
            assert!(decode_token(bad, b"test-secret").is_err());
        }
    }
}
