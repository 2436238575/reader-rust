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
    /// Access Token 有效期（秒），默认 6 小时。
    ///
    /// Access Token 是短寿命的 HS256 JWT；到期后由前端用 Refresh Token
    /// 换新，用户不会感知到固定时长的重新登录。
    pub access_token_ttl_secs: u64,
    /// Refresh Token 有效期（秒），默认 7 天，按滑动续期计算：
    /// 每次换取都会签发新令牌并重新计时，闲置超过该时长才失效。
    pub refresh_token_ttl_secs: u64,
    /// 已废弃：旧版单令牌方案的有效期。
    ///
    /// 仅为兼容既有 `.env` 保留：`JWT_TTL_SECS` 有值时会作为
    /// `ACCESS_TOKEN_TTL_SECS` 的别名生效并打警告；未显式设置新变量时
    /// 使用该值。新部署请直接配置 `ACCESS_TOKEN_TTL_SECS`。
    pub jwt_ttl_secs: u64,
    /// 唯一账号的用户名，首次启动建号时使用；建号后修改不会再改已有数据。
    pub admin_username: String,
    /// 唯一账号的密码。非空时**每次启动都会强制写入**（覆盖现有密码），
    /// 因此它也是「忘记密码」的找回通道；留空则首次启动随机生成并打印到日志。
    pub admin_password: String,
    /// 全局书架上限，`0` 表示不限制。
    pub user_book_limit: u32,
    /// 全局本地上传上限，`0` 表示不限制。
    pub user_local_book_limit: u32,
    /// 缓存容量上限（字节）；`0` 表示不限制。
    /// 缓存不再按时间过期，容量上限是唯一的自动回收手段。
    pub cache_user_limit_bytes: u64,
    /// 封面缓存目录的容量上限（字节）；`0` 表示不限制。
    pub cache_cover_limit_bytes: u64,
    /// 评论缓存的有效期（秒），默认 7 天；`0` 表示不过期。
    ///
    /// 评论是**会变的第三方数据**（新评论、点赞数、热度排序都在动），
    /// 因此这里保留时间过期，与正文缓存的「只靠容量回收」不同。
    pub review_cache_ttl_secs: u64,
    /// 评论缓存容量上限（字节）；`0` 表示不限制。
    pub review_cache_user_limit_bytes: u64,
    /// 出站请求是否允许访问私网/环回/链路本地地址。
    ///
    /// 默认 `false`（安全优先）：公网暴露的部署开箱即不会被打成内网探测代理
    /// （读取云元数据、扫内网端口）。自托管单用户场景下局域网书源、本地书源服务
    /// （如 `http://192.168.x.x:9999`）、本地模型服务属正常用法，可显式设为 `true`
    /// 整体放行，或保持 `false` 并用 `PRIVATE_NETWORK_WHITELIST` 精确放行。
    pub allow_private_network: bool,
    /// 私网白名单（逗号分隔），仅在 `ALLOW_PRIVATE_NETWORK=false` 时生效。
    ///
    /// 条目可以是 IP（`192.168.100.99`）、网段（`192.168.100.0/24`）或域名
    /// （`nas.lan`），都可带端口（`192.168.100.99:9999`——带端口时目标端口也须一致）。
    /// **名单为空时一律拦截**（不与 `ALLOW_PRIVATE_NETWORK=true` 等同）；名单非空时
    /// 只有命中的目标可以出站，其余私网/环回地址一律拦截。
    pub private_network_whitelist: String,
    /// 允许跨域访问的来源列表（逗号分隔，如 `https://a.example,https://b.example`）。
    ///
    /// 留空表示**仅同源**：不发送任何 CORS 响应头，浏览器同源策略自然生效。
    /// 前端与后端同域部署（`WEB_ROOT` 静态托管）时无需配置。
    pub cors_allowed_origins: String,
    /// 豁免登录失败限速（开发与测试环境用）。
    ///
    /// 设为 `true` 时跳过用户名维度与 IP 维度的登录失败封禁。生产部署**不应**开启。
    pub rate_limit_disabled: bool,
    /// 是否启用 Python agent sidecar（AI 资料的编排与生成）。
    ///
    /// 启用后 AI 资料更新由后端拉起 `AGENT_SIDECAR_COMMAND` 指定的 Python
    /// 进程完成；宿主机需要 Python ≥3.11（生产 Docker 镜像已内置）。
    /// 关闭后 AI 资料更新接口返回明确错误，不影响其他功能。
    pub agent_sidecar_enabled: bool,
    /// sidecar 启动命令（按空白切分程序与参数，支持双引号包裹含空格的路径）。
    ///
    /// 默认 `python -m agent_sidecar`，进程工作目录为 `sidecar/`。Windows 上
    /// `python` 可能命中 Microsoft Store 的占位符，建议配置 Python 绝对路径，
    /// 如 `C:\Python313\python.exe -m agent_sidecar`。
    pub agent_sidecar_command: String,
    /// 单章处理超时（秒），默认 600。超时即终止 sidecar 进程并中止批量任务。
    pub agent_sidecar_chapter_timeout_secs: u64,
    /// uv 二进制的显式路径。
    ///
    /// 非空时用它管理 sidecar 依赖（`uv run --project sidecar ...` 自动建
    /// `sidecar/.venv` 并安装 `pyproject.toml` 里钉死的版本）；留空时探测 PATH
    /// 里的 `uv`；都没有则降级为 `AGENT_SIDECAR_COMMAND`（需宿主机预装依赖）。
    pub agent_sidecar_uv: String,
    /// git 可执行文件的路径。
    ///
    /// 章节正文编辑版的 diff 依赖系统 git（`git diff --no-index`）。留空时用
    /// PATH 里的 `git`；装在非标准位置时在这里给绝对路径。没有 git 不影响阅读，
    /// 只是 diff 面板显示不可用原因。
    pub git_path: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_host: "127.0.0.1".to_string(),
            server_port: 8080,
            database_url: "sqlite:storage/reader.db?mode=rwc".to_string(),
            storage_dir: "storage".to_string(),
            web_root: "frontend/dist".to_string(),
            assets_dir: "storage/assets".to_string(),
            log_level: "info".to_string(),
            request_timeout_secs: 15,
            jwt_secret: String::new(),
            access_token_ttl_secs: 6 * 3600,
            refresh_token_ttl_secs: 7 * 24 * 3600,
            // 0 = 未配置；load() 里按兼容规则归一到 access_token_ttl_secs
            jwt_ttl_secs: 0,
            admin_username: "admin".to_string(),
            admin_password: String::new(),
            user_book_limit: 2000,
            user_local_book_limit: 0,
            cache_user_limit_bytes: 512 * 1024 * 1024,
            cache_cover_limit_bytes: 256 * 1024 * 1024,
            review_cache_ttl_secs: 7 * 24 * 3600,
            review_cache_user_limit_bytes: 64 * 1024 * 1024,
            allow_private_network: false,
            private_network_whitelist: String::new(),
            cors_allowed_origins: String::new(),
            rate_limit_disabled: false,
            agent_sidecar_enabled: true,
            agent_sidecar_command: "python -m agent_sidecar".to_string(),
            agent_sidecar_chapter_timeout_secs: 600,
            agent_sidecar_uv: String::new(),
            git_path: String::new(),
        }
    }
}

pub fn load() -> anyhow::Result<AppConfig> {
    // dotenvy 遇到解析不了的行会**中断加载**——之前的变量生效、之后的全丢，
    // 且这类问题有默认值兜底很难察觉。失败必须告警（含逗号等特殊字符的
    // 值要用双引号包裹），不能 .ok() 吞掉。
    // 注意这里不能用 tracing：本函数在 tracing subscriber 安装之前运行，
    // warn! 会被静默丢弃，只能走 eprintln!。
    if let Err(error) = dotenvy::dotenv() {
        eprintln!(
            "WARN .env 加载中断：{error}。该行之后的变量全部未生效——含逗号/空格等特殊字符的值请用双引号包裹"
        );
    }
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
        .set_default(
            "access_token_ttl_secs",
            defaults.access_token_ttl_secs as i64,
        )?
        .set_default(
            "refresh_token_ttl_secs",
            defaults.refresh_token_ttl_secs as i64,
        )?
        .set_default("jwt_ttl_secs", defaults.jwt_ttl_secs as i64)?
        .set_default("admin_username", defaults.admin_username)?
        .set_default("admin_password", defaults.admin_password)?
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
        .set_default(
            "private_network_whitelist",
            defaults.private_network_whitelist,
        )?
        .set_default("cors_allowed_origins", defaults.cors_allowed_origins)?
        .set_default("rate_limit_disabled", defaults.rate_limit_disabled)?
        .set_default("agent_sidecar_enabled", defaults.agent_sidecar_enabled)?
        .set_default("agent_sidecar_command", defaults.agent_sidecar_command)?
        .set_default(
            "agent_sidecar_chapter_timeout_secs",
            defaults.agent_sidecar_chapter_timeout_secs as i64,
        )?
        .set_default("agent_sidecar_uv", defaults.agent_sidecar_uv)?
        .set_default("git_path", defaults.git_path)?
        .add_source(config::Environment::default().try_parsing(true))
        .build()?;
    let mut cfg: AppConfig = cfg.try_deserialize()?;
    normalize_token_ttls(&mut cfg);
    Ok(cfg)
}

/// 归一两套令牌有效期配置。
///
/// 旧部署只有 `JWT_TTL_SECS`：未显式设置 `ACCESS_TOKEN_TTL_SECS` 时沿用它
/// 作为 Access Token 有效期，保证升级后行为可预期；两者都写时以新变量为准。
/// TTL 为 0 会让令牌签发即过期，按配置笔误处理并回落到默认值。
fn normalize_token_ttls(cfg: &mut AppConfig) {
    let defaults = AppConfig::default();
    if cfg.jwt_ttl_secs > 0 {
        if std::env::var_os("ACCESS_TOKEN_TTL_SECS").is_some() {
            eprintln!("WARN JWT_TTL_SECS 已废弃，已被 ACCESS_TOKEN_TTL_SECS 覆盖");
        } else {
            eprintln!(
                "WARN JWT_TTL_SECS 已废弃，按 ACCESS_TOKEN_TTL_SECS={} 秒生效；新部署请改用 ACCESS_TOKEN_TTL_SECS",
                cfg.jwt_ttl_secs
            );
            cfg.access_token_ttl_secs = cfg.jwt_ttl_secs;
        }
    }
    if cfg.access_token_ttl_secs == 0 {
        eprintln!(
            "WARN ACCESS_TOKEN_TTL_SECS 不能为 0，回落到默认 {} 秒",
            defaults.access_token_ttl_secs
        );
        cfg.access_token_ttl_secs = defaults.access_token_ttl_secs;
    }
    if cfg.refresh_token_ttl_secs == 0 {
        eprintln!(
            "WARN REFRESH_TOKEN_TTL_SECS 不能为 0，回落到默认 {} 秒",
            defaults.refresh_token_ttl_secs
        );
        cfg.refresh_token_ttl_secs = defaults.refresh_token_ttl_secs;
    }
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
