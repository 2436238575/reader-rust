use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct User {
    pub username: String,
    /// Argon2id PHC 字符串（自含参数与随机盐）
    pub password: String,
    #[serde(alias = "last_login_at")]
    pub last_login_at: i64,
    #[serde(alias = "created_at")]
    pub created_at: i64,
    #[serde(alias = "enable_webdav")]
    pub enable_webdav: bool,
    #[serde(alias = "enable_local_store")]
    pub enable_local_store: bool,
    #[serde(alias = "enable_ai_model")]
    pub enable_ai_model: bool,
    #[serde(alias = "is_admin")]
    pub is_admin: bool,
    /// 撤销版本号：自增即让该用户此前签发的所有 JWT 失效
    #[serde(alias = "token_version")]
    pub token_version: i64,
}
