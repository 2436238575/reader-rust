use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub server_host: String,
    pub server_port: u16,
    pub database_url: String,
    pub storage_dir: String,
    pub web_root: String,
    pub assets_dir: String,
    pub log_level: String,
    pub request_timeout_secs: u64,
    /// JWT 签名密钥。留空时启动阶段生成随机密钥并持久化到
    /// `<STORAGE_DIR>/jwt_secret`；多实例部署必须显式配置同一个值。
    #[serde(deserialize_with = "deserialize_secret")]
    pub jwt_secret: String,
    /// JWT 有效期（秒），默认 7 天。
    pub jwt_ttl_secs: u64,
    pub invite_code: String,
    pub user_limit: u32,
    pub user_book_limit: u32,
    pub user_local_book_limit: u32,
    /// 单个用户章节正文缓存的容量上限（字节）；`0` 表示不限制。
    /// 缓存不再按时间过期，容量上限是唯一的自动回收手段。
    pub cache_user_limit_bytes: u64,
    /// 封面缓存目录的容量上限（字节）；`0` 表示不限制。
    pub cache_cover_limit_bytes: u64,
    /// 评论缓存的有效期（秒），默认 7 天；`0` 表示不过期。
    ///
    /// 评论是**会变的第三方数据**（新评论、点赞数、热度排序都在动），
    /// 因此这里保留时间过期，与正文缓存的「只靠容量回收」不同。
    pub review_cache_ttl_secs: u64,
    /// 单个用户评论缓存的容量上限（字节）；`0` 表示不限制。
    pub review_cache_user_limit_bytes: u64,
    /// 出站请求是否允许访问私网/环回/链路本地地址。
    ///
    /// 默认 `true`：本项目按自托管单用户场景使用，局域网书源、本地书源服务
    /// （如 `http://192.168.x.x:9999`）、本地模型服务都属正常用法，参考实现
    /// （阅读/Legado）同样不做限制。
    ///
    /// **多用户或公网暴露的部署应设为 `false`**，否则任意用户都能把服务端
    /// 当成内网探测代理（读取云元数据、扫内网端口）。
    pub allow_private_network: bool,
    /// 允许跨域访问的来源列表（逗号分隔，如 `https://a.example,https://b.example`）。
    ///
    /// 留空表示**仅同源**：不发送任何 CORS 响应头，浏览器同源策略自然生效。
    /// 前端与后端同域部署（`WEB_ROOT` 静态托管）时无需配置。
    pub cors_allowed_origins: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_host: "0.0.0.0".to_string(),
            server_port: 8080,
            database_url: "sqlite:storage/reader.db?mode=rwc".to_string(),
            storage_dir: "storage".to_string(),
            web_root: "frontend/dist".to_string(),
            assets_dir: "storage/assets".to_string(),
            log_level: "info".to_string(),
            request_timeout_secs: 15,
            jwt_secret: String::new(),
            jwt_ttl_secs: 7 * 24 * 3600,
            invite_code: String::new(),
            user_limit: 50,
            user_book_limit: 2000,
            user_local_book_limit: 0,
            cache_user_limit_bytes: 512 * 1024 * 1024,
            cache_cover_limit_bytes: 256 * 1024 * 1024,
            review_cache_ttl_secs: 7 * 24 * 3600,
            review_cache_user_limit_bytes: 64 * 1024 * 1024,
            allow_private_network: true,
            cors_allowed_origins: String::new(),
        }
    }
}

pub fn load() -> anyhow::Result<AppConfig> {
    dotenvy::dotenv().ok();
    let defaults = AppConfig::default();
    let cfg = config::Config::builder()
        .set_default("server_host", defaults.server_host)?
        .set_default("server_port", defaults.server_port as i64)?
        .set_default("database_url", defaults.database_url)?
        .set_default("storage_dir", defaults.storage_dir)?
        .set_default("web_root", defaults.web_root)?
        .set_default("assets_dir", defaults.assets_dir)?
        .set_default("log_level", defaults.log_level)?
        .set_default("request_timeout_secs", defaults.request_timeout_secs as i64)?
        .set_default("jwt_secret", defaults.jwt_secret)?
        .set_default("jwt_ttl_secs", defaults.jwt_ttl_secs as i64)?
        .set_default("invite_code", defaults.invite_code)?
        .set_default("user_limit", defaults.user_limit as i64)?
        .set_default("user_book_limit", defaults.user_book_limit as i64)?
        .set_default(
            "user_local_book_limit",
            defaults.user_local_book_limit as i64,
        )?
        .set_default(
            "cache_user_limit_bytes",
            defaults.cache_user_limit_bytes as i64,
        )?
        .set_default(
            "cache_cover_limit_bytes",
            defaults.cache_cover_limit_bytes as i64,
        )?
        .set_default(
            "review_cache_ttl_secs",
            defaults.review_cache_ttl_secs as i64,
        )?
        .set_default(
            "review_cache_user_limit_bytes",
            defaults.review_cache_user_limit_bytes as i64,
        )?
        .set_default("allow_private_network", defaults.allow_private_network)?
        .set_default("cors_allowed_origins", defaults.cors_allowed_origins)?
        .add_source(config::Environment::default().try_parsing(true))
        .build()?;
    Ok(cfg.try_deserialize()?)
}

/// 密钥读取时容忍纯数字。
///
/// `config` 的 `try_parsing` 会把 `JWT_SECRET=123456789` 解析成整数，
/// 直接反序列化到 `String` 会导致启动失败——而随机生成的密钥恰好经常
/// 是纯数字串，这是个很容易踩的坑。
fn deserialize_secret<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Secret {
        Text(String),
        Integer(i64),
        Float(f64),
    }

    Ok(match Secret::deserialize(deserializer)? {
        Secret::Text(value) => value,
        Secret::Integer(value) => value.to_string(),
        Secret::Float(value) => value.to_string(),
    })
}
