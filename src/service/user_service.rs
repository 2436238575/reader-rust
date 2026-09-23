use crate::app::config::AppConfig;
use crate::auth::extractor::AuthUser;
use crate::auth::jwt::{encode_token, Claims};
use crate::error::error::AppError;
use crate::model::user::User;
use crate::util::crypto::{hash_password, secure_compare, verify_password};
use crate::util::time::now_ts;
use serde_json::Value;
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::fs;

/// 登录失败限速：在窗口内失败达到上限即锁定一段时间。
///
/// 此前登录接口完全没有尝试次数限制，可对已知用户名做无限次口令爆破。
const LOGIN_FAILURE_WINDOW_MS: i64 = 10 * 60 * 1000;
const LOGIN_MAX_FAILURES: usize = 8;
const LOGIN_LOCKOUT_MS: i64 = 5 * 60 * 1000;
/// 登录限速表的容量上限，防止用随机用户名打登录接口导致内存无界增长。
const MAX_LOGIN_ATTEMPT_ENTRIES: usize = 4096;

/// 单个用户名的登录失败记录（进程内，单实例部署足够）。
#[derive(Default, Clone)]
struct LoginAttempt {
    /// 窗口内的失败时间戳（毫秒）。
    failures: Vec<i64>,
    /// 锁定期截止时间；0 表示未锁定。
    locked_until: i64,
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

#[derive(Clone)]
pub struct UserService {
    cfg: AppConfig,
    data_root: PathBuf,
    cache_root: PathBuf,
    pool: SqlitePool,
    jwt_secret: Arc<Vec<u8>>,
    login_attempts: Arc<Mutex<HashMap<String, LoginAttempt>>>,
}

impl UserService {
    pub fn new(cfg: AppConfig, pool: SqlitePool, jwt_secret: Arc<Vec<u8>>) -> Self {
        let storage_root = PathBuf::from(&cfg.storage_dir);
        Self {
            data_root: storage_root.join("data"),
            cache_root: storage_root.join("cache"),
            cfg,
            pool,
            jwt_secret,
            login_attempts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 登录前置检查：单次取锁完成「锁定判断 + 本次尝试计数」。
    ///
    /// 原实现把检查与计数拆在口令校验两端，中间隔着数据库查询与 Argon2 校验的
    /// await：并发打 N 个请求可全部通过 check 后才陆续 record，实际爆破速率不受
    /// 「8 次即锁」约束。现在悲观地把每次尝试先计数、成功后再清除，并发爆发在
    /// 锁内串行化，超限请求在口令校验之前就被拒绝。
    fn login_throttle_begin(&self, username: &str) -> Result<(), AppError> {
        let mut map = self
            .login_attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let now = now_ms();
        if !map.contains_key(username) && map.len() >= MAX_LOGIN_ATTEMPT_ENTRIES {
            evict_stale_login_attempts(&mut map, now);
            if map.len() >= MAX_LOGIN_ATTEMPT_ENTRIES {
                return Err(AppError::BadRequest(
                    "登录失败次数过多，请稍后再试".to_string(),
                ));
            }
        }
        let attempt = map.entry(username.to_string()).or_default();
        attempt
            .failures
            .retain(|ts| now - *ts < LOGIN_FAILURE_WINDOW_MS);
        if attempt.locked_until > now {
            let remain_secs = (attempt.locked_until - now) / 1000 + 1;
            return Err(AppError::BadRequest(format!(
                "登录失败次数过多，请 {remain_secs} 秒后再试"
            )));
        }
        if attempt.failures.len() + 1 > LOGIN_MAX_FAILURES {
            attempt.locked_until = now + LOGIN_LOCKOUT_MS;
            attempt.failures.clear();
            return Err(AppError::BadRequest(format!(
                "登录失败次数过多，请 {} 秒后再试",
                LOGIN_LOCKOUT_MS / 1000
            )));
        }
        attempt.failures.push(now);
        Ok(())
    }

    fn login_throttle_clear(&self, username: &str) {
        let mut map = self
            .login_attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.remove(username);
    }

    /// 登录或注册，成功返回含 `accessToken`（JWT）的用户信息。
    pub async fn login(
        &self,
        username: &str,
        password: &str,
        is_login: bool,
        code: Option<&str>,
    ) -> Result<Value, AppError> {
        self.ensure_admin_user().await?;
        if is_login {
            self.login_throttle_begin(username)?;
        }
        if let Some(mut user) = self.find_user(username).await? {
            if !is_login {
                return Err(AppError::BadRequest("用户名已被占用".to_string()));
            }
            if !verify_password_async(password, &user.password).await? {
                return Err(AppError::BadRequest("密码错误".to_string()));
            }
            self.login_throttle_clear(username);
            return self.issue_login_response(&mut user).await;
        }

        if is_login {
            // 不存在的用户名同样已在 begin 中计数，不能成为无限速的探测通道
            return Err(AppError::BadRequest("用户不存在".to_string()));
        }
        self.validate_new_user(username, password, code)?;
        let user_count = self.user_count().await?;
        if user_count as u32 >= self.cfg.user_limit {
            return Err(AppError::BadRequest("超过用户数上限".to_string()));
        }

        let encrypted = hash_password_async(password).await?;
        let now = now_ms();
        let mut user = User {
            username: username.to_string(),
            password: encrypted,
            last_login_at: now,
            created_at: now,
            // 首个注册者成为管理员；ensure_admin_user 也会兜底补一个
            is_admin: user_count == 0,
            ..Default::default()
        };
        self.upsert_user_row(&user).await?;
        self.issue_login_response(&mut user).await
    }

    /// 签发令牌。纯函数，不碰数据库——撤销靠 `users.token_version` 的比对，
    /// 中间件每个请求读一次该字段，因此改权限/改密码立即生效。
    pub fn issue_token(&self, user: &User) -> Result<String, AppError> {
        let now = now_ts();
        let claims = Claims {
            sub: user.username.clone(),
            ns: user.username.clone(),
            is_admin: user.is_admin,
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
            return Ok(serde_json::json!({
                "userInfo": Value::Null,
                "adminAuthorized": false,
            }));
        };
        let info = self
            .find_user(&auth_user.username)
            .await?
            .map(|full| self.format_user(&full));
        Ok(serde_json::json!({
            "userInfo": info,
            "adminAuthorized": auth_user.is_admin,
        }))
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

    /// 用户列表。刻意不含 `accessToken`：JWT 由各自设备持有，服务端不应
    /// 也无法向管理员回吐他人的凭据。
    pub async fn get_user_list(&self) -> Result<Vec<Value>, AppError> {
        self.ensure_admin_user().await?;
        let rows = sqlx::query(
            "SELECT username, password, last_login_at, created_at, enable_webdav, \
             enable_local_store, enable_ai_model, is_admin, token_version FROM users \
             ORDER BY created_at ASC, username ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| self.format_user(&self.user_from_row(row)))
            .collect())
    }

    pub async fn add_user(&self, username: &str, password: &str) -> Result<Vec<Value>, AppError> {
        self.validate_new_user(username, password, None)?;
        if self.find_user(username).await?.is_some() {
            return Err(AppError::BadRequest("用户已存在".to_string()));
        }
        if self.user_count().await? as u32 >= self.cfg.user_limit {
            return Err(AppError::BadRequest("超过用户数上限".to_string()));
        }
        let encrypted = hash_password_async(password).await?;
        let now = now_ms();
        let user = User {
            username: username.to_string(),
            password: encrypted,
            last_login_at: now,
            created_at: now,
            ..Default::default()
        };
        self.upsert_user_row(&user).await?;
        self.get_user_list().await
    }

    /// 管理员重置他人密码；自增版本号使该用户所有已签发令牌立即失效。
    pub async fn reset_password(&self, username: &str, password: &str) -> Result<(), AppError> {
        if password.len() < 8 {
            return Err(AppError::BadRequest("密码不能低于8位".to_string()));
        }
        if username == "default" {
            return Err(AppError::BadRequest("用户不存在".to_string()));
        }
        let mut user = self
            .find_user(username)
            .await?
            .ok_or_else(|| AppError::BadRequest("用户不存在".to_string()))?;
        user.password = hash_password_async(password).await?;
        user.token_version += 1;
        self.upsert_user_row(&user).await?;
        Ok(())
    }

    /// 修改自己的密码，返回含新令牌的用户信息。
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
        let token = self.issue_token(&user)?;
        Ok(self.format_login(&user, &token))
    }

    pub async fn delete_users(&self, usernames: &[String]) -> Result<Vec<Value>, AppError> {
        for username in usernames {
            // 行删除后中间件的 load_identity 立即返回 None，无需再动版本号
            for sql in [
                "DELETE FROM users WHERE username=?1",
                "DELETE FROM book_sources WHERE user_ns=?1",
                "DELETE FROM json_documents WHERE namespace=?1",
                "DELETE FROM ai_book_memories WHERE user_ns=?1",
            ] {
                sqlx::query(sql).bind(username).execute(&self.pool).await?;
            }
            // 用户数据目录与缓存目录都要清：缓存里是已抓取的正文与封面，
            // 不清就永久留在磁盘上（此前只删了 data/，cache/ 一直泄漏）
            for dir in [
                self.data_root.join(username),
                self.cache_root.join(username),
            ] {
                if dir.exists() {
                    let _ = fs::remove_dir_all(dir).await;
                }
            }
        }
        self.get_user_list().await
    }

    pub async fn update_user(
        &self,
        username: &str,
        enable_webdav: Option<bool>,
        enable_local_store: Option<bool>,
        enable_ai_model: Option<bool>,
    ) -> Result<Vec<Value>, AppError> {
        let mut user = self
            .find_user(username)
            .await?
            .ok_or_else(|| AppError::BadRequest("用户不存在".to_string()))?;
        if let Some(v) = enable_webdav {
            user.enable_webdav = v;
        }
        if let Some(v) = enable_local_store {
            user.enable_local_store = v;
        }
        if let Some(v) = enable_ai_model {
            user.enable_ai_model = v;
        }
        self.upsert_user_row(&user).await?;
        self.get_user_list().await
    }

    /// WebDAV 的 HTTP Basic 校验。返回 `None` 表示用户名或口令不正确。
    pub async fn verify_basic_webdav(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<User>, AppError> {
        let user = match self.find_user(username).await? {
            Some(u) => u,
            None => return Ok(None),
        };
        if !user.enable_webdav {
            return Ok(None);
        }
        // 与登录接口共享同一份限速：否则这是绕开登录锁定的平行爆破通道
        self.login_throttle_begin(username)?;
        if !verify_password_async(password, &user.password).await? {
            return Ok(None);
        }
        self.login_throttle_clear(username);
        Ok(Some(user))
    }

    pub async fn find_user(&self, username: &str) -> Result<Option<User>, AppError> {
        let row = sqlx::query(
            "SELECT username, password, last_login_at, created_at, enable_webdav, \
             enable_local_store, enable_ai_model, is_admin, token_version FROM users \
             WHERE username=?1",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(|row| self.user_from_row(row)))
    }

    /// 用户列表项：不含任何凭据。
    fn format_user(&self, user: &User) -> Value {
        serde_json::json!({
            "username": user.username,
            "lastLoginAt": user.last_login_at,
            "enableWebdav": user.enable_webdav,
            "enableLocalStore": user.enable_local_store,
            "enableAiModel": user.enable_ai_model,
            "createdAt": user.created_at,
            "isAdmin": user.is_admin,
        })
    }

    /// 登录/改密码响应：在列表项基础上附带新签发的令牌。
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
            enable_webdav: row.get::<i64, _>("enable_webdav") != 0,
            enable_local_store: row.get::<i64, _>("enable_local_store") != 0,
            enable_ai_model: row.get::<i64, _>("enable_ai_model") != 0,
            is_admin: row.get::<i64, _>("is_admin") != 0,
            token_version: row.get("token_version"),
        }
    }

    async fn upsert_user_row(&self, user: &User) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO users (username, password, last_login_at, created_at, enable_webdav, \
             enable_local_store, enable_ai_model, is_admin, token_version) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(username) DO UPDATE SET password=excluded.password, \
             last_login_at=excluded.last_login_at, created_at=excluded.created_at, \
             enable_webdav=excluded.enable_webdav, enable_local_store=excluded.enable_local_store, \
             enable_ai_model=excluded.enable_ai_model, is_admin=excluded.is_admin, \
             token_version=excluded.token_version",
        )
        .bind(&user.username)
        .bind(&user.password)
        .bind(user.last_login_at)
        .bind(user.created_at)
        .bind(bool_to_i64(user.enable_webdav))
        .bind(bool_to_i64(user.enable_local_store))
        .bind(bool_to_i64(user.enable_ai_model))
        .bind(bool_to_i64(user.is_admin))
        .bind(user.token_version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn user_count(&self) -> Result<i64, AppError> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("count"))
    }

    /// 保证系统里始终有一个管理员，否则用户管理接口会永久锁死。
    async fn ensure_admin_user(&self) -> Result<(), AppError> {
        let row = sqlx::query("SELECT username FROM users WHERE is_admin=1 LIMIT 1")
            .fetch_optional(&self.pool)
            .await?;
        if row.is_some() {
            return Ok(());
        }
        let row = sqlx::query(
            "SELECT username FROM users ORDER BY CASE WHEN created_at <= 0 THEN 9223372036854775807 ELSE created_at END ASC, username ASC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = row {
            let username: String = row.get("username");
            sqlx::query("UPDATE users SET is_admin=1 WHERE username=?1")
                .bind(username)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    fn validate_new_user(
        &self,
        username: &str,
        password: &str,
        code: Option<&str>,
    ) -> Result<(), AppError> {
        if username.is_empty() {
            return Err(AppError::BadRequest("请输入用户名".to_string()));
        }
        if password.is_empty() {
            return Err(AppError::BadRequest("请输入密码".to_string()));
        }
        if username.len() < 5 {
            return Err(AppError::BadRequest("用户名不能低于5位".to_string()));
        }
        if password.len() < 8 {
            return Err(AppError::BadRequest("密码不能低于8位".to_string()));
        }
        if username == "default" {
            return Err(AppError::BadRequest("用户名不能为非法字符".to_string()));
        }
        let re = regex::Regex::new("^[a-z0-9]+$").unwrap();
        if !re.is_match(username) {
            return Err(AppError::BadRequest(
                "用户名只能由字母和数字组成".to_string(),
            ));
        }
        if !self.cfg.invite_code.is_empty() {
            let c = code.unwrap_or("");
            if c.is_empty() {
                return Err(AppError::BadRequest("请输入邀请码".to_string()));
            }
            // 常量时间比较：`==` 的逐字节短路会泄露前缀匹配长度
            if !secure_compare(c, &self.cfg.invite_code) {
                return Err(AppError::BadRequest("邀请码错误".to_string()));
            }
        }
        Ok(())
    }
}

/// 淘汰登录限速表中的死条目；仍超限时按最后活动时间最旧优先淘汰一半。
fn evict_stale_login_attempts(map: &mut HashMap<String, LoginAttempt>, now: i64) {
    map.retain(|_, a| {
        a.locked_until > now
            || a.failures
                .iter()
                .any(|ts| now - *ts < LOGIN_FAILURE_WINDOW_MS)
    });
    if map.len() >= MAX_LOGIN_ATTEMPT_ENTRIES {
        let mut by_activity: Vec<(String, i64)> = map
            .iter()
            .map(|(k, a)| {
                let last = a
                    .failures
                    .iter()
                    .copied()
                    .chain(std::iter::once(a.locked_until))
                    .max()
                    .unwrap_or(0);
                (k.clone(), last)
            })
            .collect();
        by_activity.sort_by_key(|(_, last)| *last);
        let remove_count = map.len() / 2;
        for (key, _) in by_activity.into_iter().take(remove_count) {
            map.remove(&key);
        }
    }
}

fn now_ms() -> i64 {
    now_ts() * 1000
}

fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::jwt::decode_token;
    use crate::storage::db;

    const TEST_SECRET: &[u8] = b"unit-test-secret";

    async fn create_user_service() -> (UserService, PathBuf) {
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-user-service-{}", random_suffix()));
        let cfg = AppConfig {
            storage_dir: temp_dir.to_string_lossy().to_string(),
            ..AppConfig::default()
        };
        std::fs::create_dir_all(&temp_dir).unwrap();
        let database_url = format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display());
        let pool = db::init_pool(&database_url).await.unwrap();
        let service = UserService::new(cfg, pool, Arc::new(TEST_SECRET.to_vec()));
        (service, temp_dir)
    }

    fn random_suffix() -> String {
        crate::util::crypto::random_string(8)
    }

    /// 解析登录响应里的令牌，校验签名并返回载荷。
    fn claims_of(login: &Value) -> Claims {
        let token = login["accessToken"]
            .as_str()
            .expect("登录响应应含 accessToken");
        decode_token(token, TEST_SECRET).expect("签发的令牌应可被同一密钥解出")
    }

    #[tokio::test]
    async fn login_issues_signed_token_with_identity_claims() {
        let (service, temp_dir) = create_user_service().await;

        let login = service
            .login("reader1", "password123", false, None)
            .await
            .unwrap();
        let claims = claims_of(&login);

        assert_eq!(claims.sub, "reader1");
        assert_eq!(claims.ns, "reader1");
        assert_eq!(claims.ver, 0);
        // 首个注册用户自动成为管理员
        assert!(claims.is_admin);
        assert!(claims.exp > claims.iat);
        assert_eq!(claims.exp - claims.iat, service.cfg.jwt_ttl_secs as i64);

        // 列表接口不得回吐任何凭据
        let list = service.get_user_list().await.unwrap();
        assert!(list[0].get("accessToken").is_none());

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn token_is_rejected_after_password_change_but_new_one_works() {
        let (service, temp_dir) = create_user_service().await;
        let login = service
            .login("reader1", "password123", false, None)
            .await
            .unwrap();
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
        assert!(service
            .login("reader1", "password123", true, None)
            .await
            .is_err());

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn reset_password_bumps_token_version() {
        let (service, temp_dir) = create_user_service().await;
        service
            .login("reader1", "password123", false, None)
            .await
            .unwrap();
        let before = service.find_user("reader1").await.unwrap().unwrap();

        service
            .reset_password("reader1", "resetpassword")
            .await
            .unwrap();

        let after = service.find_user("reader1").await.unwrap().unwrap();
        assert_eq!(after.token_version, before.token_version + 1);
        assert!(verify_password("resetpassword", &after.password));

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn login_throttle_locks_after_max_failures_even_under_concurrency() {
        let (service, temp_dir) = create_user_service().await;
        service
            .login("reader1", "password123", false, None)
            .await
            .unwrap();

        // 并发 20 次错误登录：check/record 分离的旧实现可以让全部请求穿过检查；
        // 悲观计数下恰好 8 次进入口令校验，其余必须在校验前被拒
        let mut handles = Vec::new();
        for _ in 0..20 {
            let svc = service.clone();
            handles.push(tokio::spawn(async move {
                svc.login("reader1", "wrong-password", true, None).await
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
        assert_eq!(verified, LOGIN_MAX_FAILURES, "恰好 8 次进入口令校验");
        assert_eq!(locked, 20 - LOGIN_MAX_FAILURES, "其余请求被限速拦截");

        // 锁定状态下正确密码同样被拒
        let err = service
            .login("reader1", "password123", true, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("失败次数过多"));

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn webdav_basic_auth_requires_the_per_user_switch() {
        let (service, temp_dir) = create_user_service().await;
        service
            .login("reader1", "password123", false, None)
            .await
            .unwrap();

        // 默认关闭
        assert!(service
            .verify_basic_webdav("reader1", "password123")
            .await
            .unwrap()
            .is_none());

        service
            .update_user("reader1", Some(true), None, None)
            .await
            .unwrap();
        assert!(service
            .verify_basic_webdav("reader1", "password123")
            .await
            .unwrap()
            .is_some());
        assert!(service
            .verify_basic_webdav("reader1", "wrong-password")
            .await
            .unwrap()
            .is_none());
        assert!(service
            .verify_basic_webdav("nobody", "password123")
            .await
            .unwrap()
            .is_none());

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn get_user_info_reports_admin_and_hides_credentials() {
        let (service, temp_dir) = create_user_service().await;
        service
            .login("reader1", "password123", false, None)
            .await
            .unwrap();

        let anonymous = service.get_user_info(None).await.unwrap();
        assert!(anonymous["userInfo"].is_null());
        assert_eq!(anonymous["adminAuthorized"], Value::Bool(false));

        let admin = AuthUser {
            username: "reader1".to_string(),
            ns: "reader1".to_string(),
            is_admin: true,
            enable_webdav: false,
            enable_ai_model: false,
            token: "test-token".to_string(),
        };
        let info = service.get_user_info(Some(&admin)).await.unwrap();
        assert_eq!(info["userInfo"]["username"], "reader1");
        assert_eq!(info["adminAuthorized"], Value::Bool(true));
        assert!(info["userInfo"].get("accessToken").is_none());
        assert!(info["userInfo"].get("password").is_none());

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn delete_user_removes_rows_and_cached_files() {
        let (service, temp_dir) = create_user_service().await;
        service
            .login("reader1", "password123", false, None)
            .await
            .unwrap();

        for sql in [
            "INSERT INTO book_sources (user_ns, book_source_url, book_source_name, json, updated_at) VALUES ('reader1', 'source-url', 'source-name', '{}', 1)",
            "INSERT INTO json_documents (namespace, name, json, updated_at) VALUES ('reader1', 'bookmark.json', '[]', 1)",
            "INSERT INTO ai_book_memories (user_ns, book_key, book_url, json, updated_at) VALUES ('reader1', 'book-key', 'book-url', '{}', 1)",
        ] {
            sqlx::query(sql).execute(&service.pool).await.unwrap();
        }
        // 缓存目录里放一个文件，验证删除用户时不会把它留下
        let cached = temp_dir.join("cache").join("reader1").join("book");
        std::fs::create_dir_all(&cached).unwrap();
        std::fs::write(cached.join("chapter.txt"), "正文").unwrap();

        service
            .delete_users(&["reader1".to_string()])
            .await
            .unwrap();

        for (sql, column) in [
            (
                "SELECT COUNT(*) AS count FROM book_sources WHERE user_ns='reader1'",
                "book_sources",
            ),
            (
                "SELECT COUNT(*) AS count FROM json_documents WHERE namespace='reader1'",
                "json_documents",
            ),
            (
                "SELECT COUNT(*) AS count FROM ai_book_memories WHERE user_ns='reader1'",
                "ai_book_memories",
            ),
            (
                "SELECT COUNT(*) AS count FROM users WHERE username='reader1'",
                "users",
            ),
        ] {
            let row = sqlx::query(sql).fetch_one(&service.pool).await.unwrap();
            assert_eq!(row.get::<i64, _>("count"), 0, "{column} should be empty");
        }
        assert!(!temp_dir.join("cache").join("reader1").exists());
        assert!(!temp_dir.join("data").join("reader1").exists());

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn permission_change_is_visible_to_the_next_request() {
        let (service, temp_dir) = create_user_service().await;
        service
            .login("reader1", "password123", false, None)
            .await
            .unwrap();
        // 第二个用户默认非管理员
        let second = service
            .login("reader2", "password123", false, None)
            .await
            .unwrap();
        assert!(!claims_of(&second).is_admin);

        service
            .update_user("reader2", None, None, Some(true))
            .await
            .unwrap();

        // 权限位由中间件每请求从库里读，不依赖令牌里的快照
        let user = service.find_user("reader2").await.unwrap().unwrap();
        assert!(user.enable_ai_model);
        assert!(!user.is_admin);

        let _ = fs::remove_dir_all(temp_dir).await;
    }

    #[tokio::test]
    async fn registration_validates_input_and_enforces_limits() {
        let (mut service, temp_dir) = create_user_service().await;
        service.cfg.invite_code = "let-me-in".to_string();

        assert!(service
            .login("reader1", "password123", false, Some("wrong"))
            .await
            .is_err());
        assert!(service
            .login("reader1", "password123", false, None)
            .await
            .is_err());
        // 邀请码正确时注册成功
        assert!(service
            .login("reader1", "password123", false, Some("let-me-in"))
            .await
            .is_ok());
        // 用户名过短、非小写字母数字、密码过短一律拒绝
        for (username, password) in [
            ("abc", "password123"),
            ("Reader1", "password123"),
            ("reader2", "short"),
        ] {
            assert!(
                service
                    .login(username, password, false, Some("let-me-in"))
                    .await
                    .is_err(),
                "{username} 应被拒绝"
            );
        }

        service.cfg.invite_code = String::new();
        service.cfg.user_limit = 1;
        assert!(service
            .login("reader9", "password123", false, None)
            .await
            .is_err());

        let _ = fs::remove_dir_all(temp_dir).await;
    }
}
