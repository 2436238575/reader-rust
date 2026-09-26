use crate::app::config::AppConfig;
use crate::auth::extractor::AuthUser;
use crate::auth::jwt::{encode_token, Claims};
use crate::error::error::AppError;
use crate::model::user::User;
use crate::util::crypto::{hash_password, random_string, verify_password};
use crate::util::hash::md5_hex;
use crate::util::time::now_ts;
use serde_json::Value;
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::fs;

/// 登录限速：用户名与 IP 双维度，进程内实现（单实例部署足够）。
///
/// - 用户名维度：登录失败 `USER_LOGIN_MAX_FAILURES` 次封禁该用户名登录 6 小时；
/// - IP 维度：登录失败 `IP_LOGIN_MAX_FAILURES` 次封禁该地址登录 6 小时。
///
/// `RATE_LIMIT_DISABLED=true` 时全部豁免（开发/测试环境）。
const USER_LOGIN_MAX_FAILURES: usize = 10;
const USER_LOGIN_BAN_MS: i64 = 6 * 60 * 60 * 1000;
const IP_LOGIN_MAX_FAILURES: usize = 5;
const IP_LOGIN_BAN_MS: i64 = 6 * 60 * 60 * 1000;
/// 限速表容量上限，防止用随机用户名/IP 打接口导致内存无界增长。
const MAX_THROTTLE_ENTRIES: usize = 4096;
/// 无封禁且超过该时长未活动的条目可被淘汰。
const THROTTLE_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 单个用户名的登录失败记录（进程内，单实例部署足够）。
#[derive(Default, Clone)]
struct UserAttempt {
    /// 自上次成功以来的失败次数（悲观计数：先计数、成功后清除）。
    failures: usize,
    /// 封禁截止时间（毫秒）；0 表示未封禁。
    banned_until: i64,
    /// 最后活动时间（毫秒），供淘汰。
    last_active: i64,
}

/// 单个 IP 的登录记录。
#[derive(Default, Clone)]
struct IpAttempt {
    login_failures: usize,
    login_banned_until: i64,
    last_active: i64,
}

#[derive(Default)]
struct AuthThrottle {
    users: HashMap<String, UserAttempt>,
    ips: HashMap<String, IpAttempt>,
}

fn remaining_secs(banned_until: i64, now: i64) -> i64 {
    (banned_until - now) / 1000 + 1
}

/// Argon2 是刻意的慢哈希（默认参数数十毫秒），统一放进 blocking 线程池，
/// 避免撞库流量占满 tokio worker 影响其他请求。
async fn hash_password_async(password: &str) -> Result<String, AppError> {
    let password = password.to_owned();
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("密码哈希任务中断: {e}")))?
        .map_err(|e| AppError::Internal(anyhow::anyhow!("密码哈希失败: {e}")))
}

async fn verify_password_async(password: &str, stored: &str) -> Result<bool, AppError> {
    let password = password.to_owned();
    let stored = stored.to_owned();
    tokio::task::spawn_blocking(move || verify_password(&password, &stored))
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("密码校验任务中断: {e}")))
}

/// 「用户不存在」分支的等时校验：对固定串做一次真实的 Argon2 验证，
/// 让该分支与「密码错误」分支的耗时对齐，堵住用响应时间枚举用户名的侧信道。
const DUMMY_HASH_INPUT: &str = "reader-rust-timing-equalizer";
static DUMMY_HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

async fn equalize_login_timing(password: &str) {
    let password = password.to_owned();
    let _ = tokio::task::spawn_blocking(move || {
        let stored = DUMMY_HASH.get_or_init(|| hash_password(DUMMY_HASH_INPUT).unwrap_or_default());
        if !stored.is_empty() {
            verify_password(&password, stored);
        }
    })
    .await;
}

#[derive(Clone)]
pub struct UserService {
    cfg: AppConfig,
    data_root: PathBuf,
    pool: SqlitePool,
    jwt_secret: Arc<Vec<u8>>,
    auth_throttle: Arc<Mutex<AuthThrottle>>,
    /// WebDAV Basic 凭据验证缓存：`(username, md5(password))` → 校验时间。
    ///
    /// Basic auth 每个请求都带全凭据，逐请求跑 Argon2（几十毫秒）在
    /// PROPFIND 目录遍历这类高频请求下开销显著。命中缓存即 5 分钟内免
    /// Argon2；改密或 `ADMIN_PASSWORD` 强制重置会立即清除对应条目。
    /// 键里不存明文密码。
    webdav_credential_cache: Arc<Mutex<HashMap<(String, String), std::time::Instant>>>,
}

/// WebDAV 凭据缓存的存活时间（秒）与条目上限。
const WEBDAV_CREDENTIAL_TTL_SECS: u64 = 300;
const WEBDAV_CREDENTIAL_MAX_ENTRIES: usize = 256;

impl UserService {
    pub fn new(cfg: AppConfig, pool: SqlitePool, jwt_secret: Arc<Vec<u8>>) -> Self {
        let storage_root = PathBuf::from(&cfg.storage_dir);
        Self {
            data_root: storage_root.join("data"),
            cfg,
            pool,
            jwt_secret,
            auth_throttle: Arc::new(Mutex::new(AuthThrottle::default())),
            webdav_credential_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 启动时保证唯一账号存在，并应用 `ADMIN_PASSWORD` 覆盖。
    ///
    /// - 账号不存在：按 `ADMIN_USERNAME` 建号；`ADMIN_PASSWORD` 非空用它，
    ///   否则随机生成强密码并打印到启动日志。
    /// - 账号已存在且 `ADMIN_PASSWORD` 非空、与当前密码不同：强制覆盖并
    ///   自增 `token_version`（旧令牌全部失效）——这是忘记密码时的找回通道；
    ///   与当前密码相同则是 no-op，避免每次重启都踢掉所有登录态。
    pub async fn bootstrap_admin(&self) -> Result<(), AppError> {
        let mut username = self.cfg.admin_username.trim().to_string();
        if username.is_empty() {
            tracing::warn!("ADMIN_USERNAME 为空，退回默认值 admin");
            username = "admin".to_string();
        }
        // 用户名就是 storage 命名空间，非法字符会塌缩成路径穿越面
        if !crate::auth::is_valid_user_ns(&username) {
            return Err(AppError::BadRequest(format!(
                "ADMIN_USERNAME 只能包含小写字母、数字与下划线（当前: {username:?}）"
            )));
        }
        let preset = self.cfg.admin_password.trim().to_string();

        if let Some(mut user) = self.find_user(&username).await? {
            if preset.is_empty() || verify_password_async(&preset, &user.password).await? {
                return Ok(());
            }
            user.password = hash_password_async(&preset).await?;
            user.token_version += 1;
            self.upsert_user_row(&user).await?;
            self.invalidate_webdav_credentials(&username);
            tracing::info!("已按 ADMIN_PASSWORD 重置账号 {username} 的密码");
            return Ok(());
        }

        let generated = preset.is_empty();
        let password = if generated { random_string(20) } else { preset };
        let now = now_ms();
        let user = User {
            username: username.clone(),
            password: hash_password_async(&password).await?,
            last_login_at: 0,
            created_at: now,
            token_version: 0,
        };
        self.upsert_user_row(&user).await?;
        if generated {
            tracing::warn!(
                "\n============================================================\n  首次启动已创建账号 {username}\n  初始随机密码：{password}\n  请登录后在「设置 → 修改密码」中修改。\n  忘记密码：设置 ADMIN_PASSWORD 环境变量后重启即可强制重置。\n============================================================"
            );
        } else {
            tracing::info!("已创建账号 {username}（密码来自 ADMIN_PASSWORD）");
        }
        Ok(())
    }

    /// 登录前置检查：单次取锁完成「封禁判断 + 本次尝试计数」。
    ///
    /// 悲观地把每次尝试先计数、成功后再清除：并发打 N 个请求在锁内串行化，
    /// 超限请求在口令校验之前就被拒绝。用户名与 IP 两个维度独立记账；
    /// `client_ip` 为 `None`（集成测试直调）时只走用户名维度。
    fn login_throttle_begin(
        &self,
        username: &str,
        client_ip: Option<&str>,
    ) -> Result<(), AppError> {
        if self.cfg.rate_limit_disabled {
            return Ok(());
        }
        let now = now_ms();
        let mut throttle = self.auth_throttle.lock().unwrap_or_else(|e| e.into_inner());
        evict_stale_throttle_entries(&mut throttle, now);

        // 用户名维度：失败 10 次封 6 小时
        if throttle.users.len() >= MAX_THROTTLE_ENTRIES && !throttle.users.contains_key(username) {
            return Err(AppError::BadRequest(
                "登录失败次数过多，请稍后再试".to_string(),
            ));
        }
        let user = throttle.users.entry(username.to_string()).or_default();
        if user.banned_until > now {
            return Err(AppError::BadRequest(format!(
                "登录失败次数过多，请 {} 秒后再试",
                remaining_secs(user.banned_until, now)
            )));
        }
        if user.failures + 1 > USER_LOGIN_MAX_FAILURES {
            user.banned_until = now + USER_LOGIN_BAN_MS;
            user.failures = 0;
            user.last_active = now;
            return Err(AppError::BadRequest(format!(
                "登录失败次数过多，请 {} 秒后再试",
                USER_LOGIN_BAN_MS / 1000
            )));
        }
        user.failures += 1;
        user.last_active = now;

        // IP 维度：失败 5 次封该地址 6 小时
        let Some(ip) = client_ip else {
            return Ok(());
        };
        if let Some(entry) = throttle.ips.get_mut(ip) {
            if entry.login_banned_until > now {
                return Err(AppError::BadRequest(format!(
                    "该地址登录失败次数过多，请 {} 秒后再试",
                    remaining_secs(entry.login_banned_until, now)
                )));
            }
        } else if throttle.ips.len() >= MAX_THROTTLE_ENTRIES {
            // IP 表满：放行用户维度的判定，不再新增条目
            return Ok(());
        }
        let ip_entry = throttle.ips.entry(ip.to_string()).or_default();
        if ip_entry.login_failures + 1 > IP_LOGIN_MAX_FAILURES {
            ip_entry.login_banned_until = now + IP_LOGIN_BAN_MS;
            ip_entry.login_failures = 0;
            ip_entry.last_active = now;
            return Err(AppError::BadRequest(format!(
                "该地址登录失败次数过多，请 {} 秒后再试",
                IP_LOGIN_BAN_MS / 1000
            )));
        }
        ip_entry.login_failures += 1;
        ip_entry.last_active = now;
        Ok(())
    }

    /// 登录成功后的清账：移除用户名条目、清零该 IP 的登录失败计数。
    fn login_throttle_clear(&self, username: &str, client_ip: Option<&str>) {
        if self.cfg.rate_limit_disabled {
            return;
        }
        let now = now_ms();
        let mut throttle = self.auth_throttle.lock().unwrap_or_else(|e| e.into_inner());
        throttle.users.remove(username);
        if let Some(ip) = client_ip {
            if let Some(entry) = throttle.ips.get_mut(ip) {
                entry.login_failures = 0;
                entry.last_active = now;
            }
        }
    }

    /// 登录，成功返回含 `accessToken`（JWT）的用户信息。
    ///
    /// `client_ip` 来自 `ConnectInfo`：IP 维度的登录封禁按它记账；集成测试等
    /// 拿不到对端地址的场景传 `None`，此时只走用户名维度的限速。
    pub async fn login(
        &self,
        username: &str,
        password: &str,
        client_ip: Option<&str>,
    ) -> Result<Value, AppError> {
        self.login_throttle_begin(username, client_ip)?;
        if let Some(mut user) = self.find_user(username).await? {
            if !verify_password_async(password, &user.password).await? {
                return Err(AppError::BadRequest("用户名或密码错误".to_string()));
            }
            self.login_throttle_clear(username, client_ip);
            return self.issue_login_response(&mut user).await;
        }

        // 不存在的用户名同样已在 begin 中计数，不能成为无限速的探测通道；
        // 文案与「密码错误」统一，并补一次等时校验，防时序侧信道枚举
        equalize_login_timing(password).await;
        Err(AppError::BadRequest("用户名或密码错误".to_string()))
    }

    /// 签发令牌。纯函数，不碰数据库——撤销靠 `users.token_version` 的比对，
    /// 中间件每个请求读一次该字段，因此改密码立即生效。
    pub fn issue_token(&self, user: &User) -> Result<String, AppError> {
        let now = now_ts();
        let claims = Claims {
            sub: user.username.clone(),
            ns: user.username.clone(),
            iat: now,
            exp: now.saturating_add(self.cfg.jwt_ttl_secs as i64),
            ver: user.token_version,
        };
        encode_token(&claims, &self.jwt_secret)
    }

    async fn issue_login_response(&self, user: &mut User) -> Result<Value, AppError> {
        let now = now_ms();
        user.last_login_at = now;
        sqlx::query("UPDATE users SET last_login_at=?1 WHERE username=?2")
            .bind(now)
            .bind(&user.username)
            .execute(&self.pool)
            .await?;
        let token = self.issue_token(user)?;
        Ok(self.format_login(user, &token))
    }

    /// `getUserInfo` 的载荷。身份已由 `optional_auth` 中间件解析，这里只做补全。
    pub async fn get_user_info(&self, user: Option<&AuthUser>) -> Result<Value, AppError> {
        let Some(auth_user) = user else {
            return Ok(serde_json::json!({ "userInfo": Value::Null }));
        };
        let info = self
            .find_user(&auth_user.username)
            .await?
            .map(|full| self.format_user(&full));
        Ok(serde_json::json!({ "userInfo": info }))
    }

    pub async fn save_user_config(&self, user_ns: &str, config: Value) -> Result<(), AppError> {
        let dir = self.data_root.join(user_ns);
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let path = dir.join("userConfig.json");
        let mut cfg = config;
        if let Some(obj) = cfg.as_object_mut() {
            obj.insert("@updateTime".to_string(), Value::from(now_ts() * 1000));
        }
        let data = serde_json::to_string(&cfg).map_err(|e| AppError::BadRequest(e.to_string()))?;
        fs::write(path, data)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        Ok(())
    }

    pub async fn get_user_config(&self, user_ns: &str) -> Result<Value, AppError> {
        let path = self.data_root.join(user_ns).join("userConfig.json");
        if !path.exists() {
            return Err(AppError::BadRequest("没有备份文件".to_string()));
        }
        let data = fs::read_to_string(path)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let v: Value =
            serde_json::from_str(&data).map_err(|e| AppError::BadRequest(e.to_string()))?;
        Ok(v)
    }

    /// 修改密码，返回含新令牌的用户信息。
    ///
    /// 版本号自增会让所有旧令牌失效（等价于「改密码踢掉其他设备」），
    /// 同时为当前设备换发一个新令牌，避免用户改完密码立刻被登出。
    pub async fn change_password(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<Value, AppError> {
        if new_password.len() < 8 {
            return Err(AppError::BadRequest("密码不能低于8位".to_string()));
        }
        let mut user = self
            .find_user(username)
            .await?
            .ok_or_else(|| AppError::BadRequest("用户不存在".to_string()))?;
        if !verify_password_async(old_password, &user.password).await? {
            return Err(AppError::BadRequest("当前密码错误".to_string()));
        }

        user.password = hash_password_async(new_password).await?;
        user.token_version += 1;
        user.last_login_at = now_ms();
        self.upsert_user_row(&user).await?;
        self.invalidate_webdav_credentials(username);
        let token = self.issue_token(&user)?;
        Ok(self.format_login(&user, &token))
    }

    /// WebDAV 的 HTTP Basic 校验。返回 `None` 表示用户名或口令不正确。
    pub async fn verify_basic_webdav(
        &self,
        username: &str,
        password: &str,
        client_ip: Option<&str>,
    ) -> Result<Option<User>, AppError> {
        let user = match self.find_user(username).await? {
            Some(u) => u,
            None => return Ok(None),
        };
        let credential_key = (username.to_string(), md5_hex(password));
        if self.webdav_credential_cached(&credential_key) {
            return Ok(Some(user));
        }
        // 与登录接口共享同一份限速：否则这是绕开登录锁定的平行爆破通道
        self.login_throttle_begin(username, client_ip)?;
        if !verify_password_async(password, &user.password).await? {
            return Ok(None);
        }
        self.login_throttle_clear(username, client_ip);
        self.remember_webdav_credential(credential_key);
        Ok(Some(user))
    }

    fn webdav_credential_cached(&self, key: &(String, String)) -> bool {
        let mut cache = self
            .webdav_credential_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(&verified_at) = cache.get(key) else {
            return false;
        };
        if verified_at.elapsed().as_secs() > WEBDAV_CREDENTIAL_TTL_SECS {
            cache.remove(key);
            return false;
        }
        true
    }

    fn remember_webdav_credential(&self, key: (String, String)) {
        let mut cache = self
            .webdav_credential_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if cache.len() >= WEBDAV_CREDENTIAL_MAX_ENTRIES {
            // 先清过期，仍满则整表清空（与 auth_throttle 同策略）
            cache.retain(|_, verified_at| {
                verified_at.elapsed().as_secs() <= WEBDAV_CREDENTIAL_TTL_SECS
            });
            if cache.len() >= WEBDAV_CREDENTIAL_MAX_ENTRIES {
                cache.clear();
            }
        }
        cache.insert(key, std::time::Instant::now());
    }

    /// 清除某用户的 WebDAV 凭据缓存（改密/强制重置后旧凭据必须重新验证）。
    fn invalidate_webdav_credentials(&self, username: &str) {
        self.webdav_credential_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(user, _), _| user != username);
    }

    pub async fn find_user(&self, username: &str) -> Result<Option<User>, AppError> {
        let row = sqlx::query(
            "SELECT username, password, last_login_at, created_at, token_version FROM users \
             WHERE username=?1",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(|row| self.user_from_row(row)))
    }

    /// 对外返回的用户信息：不含任何凭据。
    fn format_user(&self, user: &User) -> Value {
        serde_json::json!({
            "username": user.username,
            "lastLoginAt": user.last_login_at,
            "createdAt": user.created_at,
        })
    }

    /// 登录/改密码响应：在用户信息基础上附带新签发的令牌。
    fn format_login(&self, user: &User, token: &str) -> Value {
        let mut value = self.format_user(user);
        if let Some(obj) = value.as_object_mut() {
            obj.insert("accessToken".to_string(), Value::from(token));
        }
        value
    }

    fn user_from_row(&self, row: &SqliteRow) -> User {
        User {
            username: row.get("username"),
            password: row.get("password"),
            last_login_at: row.get("last_login_at"),
            created_at: row.get("created_at"),
            token_version: row.get("token_version"),
        }
    }

    async fn upsert_user_row(&self, user: &User) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO users (username, password, last_login_at, created_at, token_version) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(username) DO UPDATE SET password=excluded.password, \
             last_login_at=excluded.last_login_at, created_at=excluded.created_at, \
             token_version=excluded.token_version",
        )
        .bind(&user.username)
        .bind(&user.password)
        .bind(user.last_login_at)
        .bind(user.created_at)
        .bind(user.token_version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// 淘汰限速表中的死条目：无活动封禁且超过保留时长未活动的才清。
///
/// 仅在表达到容量上限时调用（`login_throttle_begin` 的 len 预检），平时零开销。
fn evict_stale_throttle_entries(throttle: &mut AuthThrottle, now: i64) {
    if throttle.users.len() < MAX_THROTTLE_ENTRIES && throttle.ips.len() < MAX_THROTTLE_ENTRIES {
        return;
    }
    throttle
        .users
        .retain(|_, a| a.banned_until > now || now - a.last_active < THROTTLE_RETENTION_MS);
    throttle
        .ips
        .retain(|_, a| a.login_banned_until > now || now - a.last_active < THROTTLE_RETENTION_MS);
}

fn now_ms() -> i64 {
    now_ts() * 1000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::jwt::decode_token;
    use crate::storage::db;

    const TEST_SECRET: &[u8] = b"unit-test-secret";

    /// 以预设凭据建一个账号为 reader1/password123 的服务实例。
    async fn create_user_service() -> (UserService, PathBuf) {
        create_user_service_with("reader1", "password123").await
    }

    async fn create_user_service_with(username: &str, password: &str) -> (UserService, PathBuf) {
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-user-service-{}", random_suffix()));
        let cfg = AppConfig {
            admin_username: username.to_string(),
            admin_password: password.to_string(),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            ..AppConfig::default()
        };
        std::fs::create_dir_all(&temp_dir).unwrap();
        let database_url = format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display());
        let pool = db::init_pool(&database_url).await.unwrap();
        let service = UserService::new(cfg, pool, Arc::new(TEST_SECRET.to_vec()));
        service.bootstrap_admin().await.unwrap();
        (service, temp_dir)
    }

    fn random_suffix() -> String {
        random_string(8)
    }

    /// 解析登录响应里的令牌，校验签名并返回载荷。
    fn claims_of(login: &Value) -> Claims {
        let token = login["accessToken"]
            .as_str()
            .expect("登录响应应含 accessToken");
        decode_token(token, TEST_SECRET).expect("签发的令牌应可被同一密钥解出")
    }

    #[tokio::test]
    async fn bootstrap_generates_random_password_when_none_preset() {
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-bootstrap-{}", random_suffix()));
        let cfg = AppConfig {
            admin_username: "reader1".to_string(),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            ..AppConfig::default()
        };
        std::fs::create_dir_all(&temp_dir).unwrap();
        let database_url = format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display());
        let pool = db::init_pool(&database_url).await.unwrap();
        let service = UserService::new(cfg, pool, Arc::new(TEST_SECRET.to_vec()));

        service.bootstrap_admin().await.unwrap();
        let user = service.find_user("reader1").await.unwrap().unwrap();
        assert!(user.password.starts_with("$argon2id$"));
        assert_eq!(user.token_version, 0);

        // 幂等：再次 bootstrap（仍无 ADMIN_PASSWORD）不动密码
        service.bootstrap_admin().await.unwrap();
        let again = service.find_user("reader1").await.unwrap().unwrap();
        assert_eq!(again.password, user.password);

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn preset_admin_password_overrides_only_when_changed() {
        let (mut service, temp_dir) = create_user_service_with("reader1", "password123").await;
        service.login("reader1", "password123", None).await.unwrap();
        let before = service.find_user("reader1").await.unwrap().unwrap();

        // 相同的预设密码是 no-op：令牌不失效、哈希不变
        service.bootstrap_admin().await.unwrap();
        let same = service.find_user("reader1").await.unwrap().unwrap();
        assert_eq!(same.token_version, before.token_version);
        assert_eq!(same.password, before.password);

        // 预设密码变化：覆盖并自增版本号，旧口令失效
        service.cfg.admin_password = "newpassword456".to_string();
        service.bootstrap_admin().await.unwrap();
        let after = service.find_user("reader1").await.unwrap().unwrap();
        assert_eq!(after.token_version, before.token_version + 1);
        assert!(service.login("reader1", "password123", None).await.is_err());
        service
            .login("reader1", "newpassword456", None)
            .await
            .unwrap();

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn bootstrap_rejects_invalid_admin_username() {
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-badname-{}", random_suffix()));
        let cfg = AppConfig {
            admin_username: "../evil".to_string(),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            ..AppConfig::default()
        };
        std::fs::create_dir_all(&temp_dir).unwrap();
        let database_url = format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display());
        let pool = db::init_pool(&database_url).await.unwrap();
        let service = UserService::new(cfg, pool, Arc::new(TEST_SECRET.to_vec()));

        let err = service.bootstrap_admin().await.unwrap_err();
        assert!(err.to_string().contains("ADMIN_USERNAME"));

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn login_issues_signed_token_with_identity_claims() {
        let (service, temp_dir) = create_user_service().await;

        let login = service.login("reader1", "password123", None).await.unwrap();
        let claims = claims_of(&login);

        assert_eq!(claims.sub, "reader1");
        assert_eq!(claims.ns, "reader1");
        assert_eq!(claims.ver, 0);
        assert!(claims.exp > claims.iat);
        assert_eq!(claims.exp - claims.iat, service.cfg.jwt_ttl_secs as i64);

        // 对外响应不得回吐凭据
        assert!(login.get("password").is_none());
        let info = service.get_user_info(None).await.unwrap();
        assert!(info["userInfo"].is_null());

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn token_is_rejected_after_password_change_but_new_one_works() {
        let (service, temp_dir) = create_user_service().await;
        let login = service.login("reader1", "password123", None).await.unwrap();
        let old_claims = claims_of(&login);

        let changed = service
            .change_password("reader1", "password123", "newpassword123")
            .await
            .unwrap();
        let new_claims = claims_of(&changed);

        // 版本号自增即撤销：旧令牌的 ver 不再匹配
        assert_eq!(new_claims.ver, old_claims.ver + 1);
        assert_ne!(old_claims.ver, new_claims.ver);
        // 改密码后旧口令失效
        assert!(service.login("reader1", "password123", None).await.is_err());

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn login_throttle_locks_after_max_failures_even_under_concurrency() {
        let (service, temp_dir) = create_user_service().await;

        // 并发 20 次错误登录：check/record 分离的旧实现可以让全部请求穿过检查；
        // 悲观计数下恰好 10 次进入口令校验，其余必须在校验前被拒
        let mut handles = Vec::new();
        for _ in 0..20 {
            let svc = service.clone();
            handles.push(tokio::spawn(async move {
                svc.login("reader1", "wrong-password", None).await
            }));
        }
        let mut verified = 0;
        let mut locked = 0;
        for h in handles {
            match h.await.unwrap() {
                Err(AppError::BadRequest(msg)) if msg.contains("失败次数过多") => locked += 1,
                Err(AppError::BadRequest(msg)) if msg.contains("密码错误") => verified += 1,
                other => panic!("错误密码登录的返回异常: {other:?}"),
            }
        }
        assert_eq!(verified, USER_LOGIN_MAX_FAILURES, "恰好 10 次进入口令校验");
        assert_eq!(locked, 20 - USER_LOGIN_MAX_FAILURES, "其余请求被限速拦截");

        // 锁定状态下正确密码同样被拒
        let err = service
            .login("reader1", "password123", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("失败次数过多"));

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn ip_login_failures_ban_the_address_for_six_hours() {
        let (service, temp_dir) = create_user_service().await;

        // 同一地址错 5 次（用户名维度上限 10 尚未触达）
        for _ in 0..5 {
            let err = service
                .login("reader1", "wrong-password", Some("203.0.113.9"))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("用户名或密码错误"));
        }
        // 第 6 次起封该地址：连正确密码也拒
        let err = service
            .login("reader1", "password123", Some("203.0.113.9"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("该地址登录失败次数过多"));
        // 其他地址不受影响
        service
            .login("reader1", "password123", Some("198.51.100.7"))
            .await
            .unwrap();

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn user_login_failures_ban_the_username_for_six_hours() {
        let (service, temp_dir) = create_user_service().await;

        // client_ip=None：只走用户名维度，错满 10 次
        for _ in 0..USER_LOGIN_MAX_FAILURES {
            let err = service
                .login("reader1", "wrong-password", None)
                .await
                .unwrap_err();
            assert!(err.to_string().contains("用户名或密码错误"));
        }
        // 封禁后正确密码也拒
        let err = service
            .login("reader1", "password123", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("登录失败次数过多"));
        // 封禁按用户名记账：换其他用户名（不存在）报的是「密码错误」而非封禁
        let err = service
            .login("nobody", "password123", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("用户名或密码错误"));

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn rate_limit_disabled_skips_login_throttles() {
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-nolimit-{}", random_suffix()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = AppConfig {
            rate_limit_disabled: true,
            admin_username: "reader1".to_string(),
            admin_password: "password123".to_string(),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            ..AppConfig::default()
        };
        let database_url = format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display());
        let pool = db::init_pool(&database_url).await.unwrap();
        let service = UserService::new(cfg, pool, Arc::new(TEST_SECRET.to_vec()));
        service.bootstrap_admin().await.unwrap();

        // 超过用户与 IP 两维度的失败上限，均不触发封禁
        for _ in 0..15 {
            let err = service
                .login("reader1", "wrong-password", Some("203.0.113.9"))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("用户名或密码错误"));
        }

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn webdav_basic_auth_verifies_credentials() {
        let (service, temp_dir) = create_user_service().await;

        assert!(service
            .verify_basic_webdav("reader1", "password123", None)
            .await
            .unwrap()
            .is_some());
        assert!(service
            .verify_basic_webdav("reader1", "wrong-password", None)
            .await
            .unwrap()
            .is_none());
        assert!(service
            .verify_basic_webdav("nobody", "password123", None)
            .await
            .unwrap()
            .is_none());

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn get_user_info_hides_credentials() {
        let (service, temp_dir) = create_user_service().await;

        let anonymous = service.get_user_info(None).await.unwrap();
        assert!(anonymous["userInfo"].is_null());

        let auth = AuthUser {
            username: "reader1".to_string(),
            ns: "reader1".to_string(),
            token: "test-token".to_string(),
        };
        let info = service.get_user_info(Some(&auth)).await.unwrap();
        assert_eq!(info["userInfo"]["username"], "reader1");
        assert!(info["userInfo"].get("accessToken").is_none());
        assert!(info["userInfo"].get("password").is_none());

        let _ = fs::remove_dir_all(temp_dir).await;
    }
}
