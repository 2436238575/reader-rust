//! JWT 鉴权：令牌编解码、签名密钥解析、请求级身份提取与路由中间件。
//!
//! 与 `service` 的依赖是单向的：`service::user_service` 用 [`jwt`] 签发令牌，
//! 本模块不反向依赖任何 service——中间件自己持有一条只读的身份查询，
//! 用来校验撤销版本号。

pub mod extractor;
pub mod jwt;
pub mod middleware;
pub mod secret;

use sqlx::{Row, SqlitePool};
use std::sync::Arc;

pub use extractor::{CurrentUser, MaybeUser};
pub use middleware::{optional_auth, require_auth};
pub use secret::resolve_jwt_secret;

/// 中间件所需的共享状态：签名密钥 + 校验撤销版本号用的连接池。
#[derive(Clone)]
pub struct AuthState {
    pool: SqlitePool,
    jwt_secret: Arc<Vec<u8>>,
}

impl AuthState {
    pub fn new(pool: SqlitePool, jwt_secret: Arc<Vec<u8>>) -> Self {
        Self { pool, jwt_secret }
    }

    pub fn jwt_secret(&self) -> &[u8] {
        &self.jwt_secret
    }

    /// 读取用户的撤销版本号；用户不存在返回 `None`。
    ///
    /// 走主键查询，是每个受保护请求唯一的一次数据库往返。
    pub async fn load_token_version(&self, username: &str) -> Result<Option<i64>, sqlx::Error> {
        let row = sqlx::query("SELECT token_version FROM users WHERE username=?1")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| row.get("token_version")))
    }
}

/// 用户命名空间的合法形式：与注册用户名同字符集（`^[a-z0-9]+$`），
/// 另允许下划线以兼容内部命名（`__default__`、`__app__`）。
///
/// 命名空间直接参与 storage 路径拼接（书架、缓存、上传目录等），
/// 因此任何来源的 ns 都必须先过这里，否则可借 `../..` 穿越数据目录。
pub fn is_valid_user_ns(ns: &str) -> bool {
    !ns.is_empty()
        && ns.len() <= 64
        && ns
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_ns_accepts_plain_and_internal_names() {
        assert!(is_valid_user_ns("reader1"));
        assert!(is_valid_user_ns("__default__"));
        assert!(is_valid_user_ns("__app__"));
        assert!(is_valid_user_ns("a_b_1"));
    }

    #[test]
    fn user_ns_rejects_path_like_and_non_ascii_values() {
        for bad in ["", "../data", "..\\..\\x", "a/b", "a.b", "UPPER", "读者"] {
            assert!(!is_valid_user_ns(bad), "{bad:?} 应被拒绝");
        }
        assert!(!is_valid_user_ns(&"a".repeat(65)));
    }
}
