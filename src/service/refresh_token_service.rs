//! Refresh Token 的签发、一次性轮换与吊销。
//!
//! 令牌本体是 `rt_` 前缀的高熵随机串，明文只在签发时下发一次；数据库只存
//! SHA-256 摘要，泄露数据库也无法还原可用令牌。一次登录创建一条 family，
//! 每次换取都在同一 family 下追加新行并给旧行打 `used_at` 标记——旧行保留
//! 不删，用来识别重放：已消费令牌在宽限窗口外再次出现即吊销整条 family。
//!
//! 轮换（`rotate`）全程在 BEGIN IMMEDIATE 事务里执行：读、插新行、消费旧行、
//! 清理过期行与吊销家族对写锁互斥——并发的登出/改密吊销提交后，轮换必然
//! 读到 `revoked_at`，不会再长出不在吊销集合里的新行（用户已登出却多出一枚
//! 有效令牌）。sqlx 的 `Transaction` 默认 DEFERRED，必须显式
//! `begin_with("BEGIN IMMEDIATE")`（见 `db::init_pool` 的说明）。

use crate::error::error::AppError;
use crate::util::time::now_ts;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};

/// 令牌前缀：与 JWT 明确区分，避免被误放进 `Authorization` 头。
const TOKEN_PREFIX: &str = "rt_";
/// 随机部分 32 字节（16 进制编码后 64 字符），足以抵御离线猜测。
const TOKEN_BYTES: usize = 32;
/// 一次性令牌的容错窗口（秒）。
///
/// 客户端收到轮换响应前断网、或两个标签页同时刷新时，已消费令牌仍可再次
/// 换取（同一 family，原轮换出的令牌继续有效）。超过该窗口才视为重放。
const REUSE_GRACE_SECS: i64 = 30;
/// 拒绝超长输入参与哈希；合法令牌固定远小于该值。
const MAX_TOKEN_LEN: usize = 128;

/// 轮换成功后的身份信息：Access Token 仍按原 claims 签发。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshedSession {
    pub uid: i64,
    pub username: String,
    pub token_version: i64,
    pub refresh_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotateOutcome {
    Rotated(RefreshedSession),
    /// 不存在 / 已过期 / 已吊销 / 撤销版本不匹配。
    Invalid,
    /// 已消费令牌在宽限窗口外再次出现；整条 family 已吊销。
    ReuseDetected,
}

#[derive(Clone)]
pub struct RefreshTokenStore {
    pool: SqlitePool,
    ttl_secs: u64,
}

impl RefreshTokenStore {
    pub fn new(pool: SqlitePool, ttl_secs: u64) -> Self {
        Self { pool, ttl_secs }
    }

    /// 新登录 / 改密：创建一条全新的会话 family。
    pub async fn issue(&self, uid: i64, token_version: i64) -> Result<String, AppError> {
        let family_id = uuid::Uuid::new_v4().simple().to_string();
        let token = self
            .insert_row(uid, token_version, &family_id)
            .await?
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("新会话家族创建失败")))?;
        self.purge_expired().await?;
        Ok(token)
    }

    /// 用 Refresh Token 换一对新令牌。
    ///
    /// 全程持有写锁（BEGIN IMMEDIATE）：SELECT、插新行、消费旧行、purge 都在
    /// 同一事务内，与 `revoke_family_by_token`（登出）/ `revoke_all_for_user`
    /// （改密）串行化。吊销先提交则这里读到 `revoked_at` 直接 Invalid；
    /// 轮换先提交则后续吊销会把新行一并纳入家族——两种次序下都不会漏。
    pub async fn rotate(&self, raw: &str) -> Result<RotateOutcome, AppError> {
        let raw = raw.trim();
        if !is_well_formed(raw) {
            return Ok(RotateOutcome::Invalid);
        }
        let token_hash = hash_token(raw);
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT r.uid, r.token_version, r.family_id, r.expires_at, r.used_at, r.revoked_at, \
                    u.username, u.token_version AS current_version \
             FROM refresh_tokens r JOIN users u ON u.uid = r.uid \
             WHERE r.token_hash=?1",
        )
        .bind(&token_hash)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(RotateOutcome::Invalid);
        };

        let uid: i64 = row.get("uid");
        let username: String = row.get("username");
        let token_version: i64 = row.get("token_version");
        let current_version: i64 = row.get("current_version");
        let family_id: String = row.get("family_id");
        let expires_at: i64 = row.get("expires_at");
        let used_at: Option<i64> = row.get("used_at");
        let revoked_at: Option<i64> = row.get("revoked_at");
        let now = now_ts();

        if revoked_at.is_some() || expires_at <= now {
            tx.rollback().await?;
            return Ok(RotateOutcome::Invalid);
        }
        // 改密码会自增 token_version；即使吊销行没走完，这里也兜底拒绝
        if token_version != current_version {
            revoke_family_on(&mut tx, &family_id).await?;
            tx.commit().await?;
            return Ok(RotateOutcome::Invalid);
        }

        let Some(used_at) = used_at else {
            // 先插新行、再消费旧行：同一事务内原子提交。新行若发现家族已被
            // 吊销（继承 revoked_at 的兜底），按无效处理。
            let Some(token) =
                insert_row_on(&mut tx, uid, token_version, &family_id, self.ttl_secs).await?
            else {
                tx.commit().await?;
                return Ok(RotateOutcome::Invalid);
            };
            let updated = sqlx::query(
                "UPDATE refresh_tokens SET used_at=?1 WHERE token_hash=?2 AND used_at IS NULL",
            )
            .bind(now)
            .bind(&token_hash)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() != 1 {
                // 写锁在手，读到的 used_at 不可能被别人改掉，这里只是防御
                tx.rollback().await?;
                return Ok(RotateOutcome::Invalid);
            }
            purge_expired_on(&mut tx).await?;
            tx.commit().await?;
            return Ok(RotateOutcome::Rotated(RefreshedSession {
                uid,
                username,
                token_version,
                refresh_token: token,
            }));
        };

        if now.saturating_sub(used_at) > REUSE_GRACE_SECS {
            revoke_family_on(&mut tx, &family_id).await?;
            tx.commit().await?;
            tracing::warn!(
                uid,
                family_id,
                "refresh token 在宽限窗口外被再次使用，已吊销整条会话家族"
            );
            return Ok(RotateOutcome::ReuseDetected);
        }
        // 宽限窗口内的重复换取（断网重试/双标签页并发）：再发一枚，
        // 原轮换出的令牌继续有效
        let Some(token) =
            insert_row_on(&mut tx, uid, token_version, &family_id, self.ttl_secs).await?
        else {
            tx.commit().await?;
            return Ok(RotateOutcome::Invalid);
        };
        purge_expired_on(&mut tx).await?;
        tx.commit().await?;
        Ok(RotateOutcome::Rotated(RefreshedSession {
            uid,
            username,
            token_version,
            refresh_token: token,
        }))
    }

    /// 登出：吊销该令牌所属的整条 family（即这次登录会话）。
    ///
    /// 令牌已过期/不存在时返回 `false`，调用方仍按登出成功处理。
    pub async fn revoke_family_by_token(&self, raw: &str) -> Result<bool, AppError> {
        let raw = raw.trim();
        if !is_well_formed(raw) {
            return Ok(false);
        }
        let row = sqlx::query("SELECT family_id FROM refresh_tokens WHERE token_hash=?1")
            .bind(hash_token(raw))
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let family_id: String = row.get("family_id");
        self.revoke_family(&family_id).await?;
        Ok(true)
    }

    /// 改密码 / ADMIN_PASSWORD 强制重置：作废该用户全部会话。
    pub async fn revoke_all_for_user(&self, uid: i64) -> Result<u64, AppError> {
        let result = sqlx::query(
            "UPDATE refresh_tokens SET revoked_at=?1 WHERE uid=?2 AND revoked_at IS NULL",
        )
        .bind(now_ts())
        .bind(uid)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// 清理过期行；已消费但未过期的行保留，用于重放检测。
    pub async fn purge_expired(&self) -> Result<u64, AppError> {
        let result = sqlx::query("DELETE FROM refresh_tokens WHERE expires_at <= ?1")
            .bind(now_ts())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn revoke_family(&self, family_id: &str) -> Result<(), AppError> {
        let mut conn = self.pool.acquire().await?;
        revoke_family_on(&mut conn, family_id).await
    }

    /// 插入一条新令牌行（池连接版，供 `issue` 使用）。
    ///
    /// `revoked_at` 从同 family 的已有行继承：如果家族在并发中被吊销，新行
    /// 会立即带上吊销时间，不会在吊销之后又长出可用令牌。返回 `None` 表示
    /// 家族已被吊销，调用方按无效处理（新家族不会命中该分支）。
    async fn insert_row(
        &self,
        uid: i64,
        token_version: i64,
        family_id: &str,
    ) -> Result<Option<String>, AppError> {
        let mut conn = self.pool.acquire().await?;
        insert_row_on(&mut conn, uid, token_version, family_id, self.ttl_secs).await
    }
}

/// 吊销家族内全部未吊销行（事务内版）。
async fn revoke_family_on(
    conn: &mut sqlx::SqliteConnection,
    family_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE refresh_tokens SET revoked_at=?1 WHERE family_id=?2 AND revoked_at IS NULL",
    )
    .bind(now_ts())
    .bind(family_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// 清理过期行（事务内版）；已消费但未过期的行保留，用于重放检测。
async fn purge_expired_on(conn: &mut sqlx::SqliteConnection) -> Result<(), AppError> {
    sqlx::query("DELETE FROM refresh_tokens WHERE expires_at <= ?1")
        .bind(now_ts())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// 插入一条新令牌行（事务/连接内版）。
///
/// `revoked_at` 从同 family 的已有行继承：如果家族已被吊销，新行会立即
/// 带上吊销时间，不会在吊销之后又长出可用令牌。返回 `None` 表示家族已被
/// 吊销，调用方按无效处理（新家族不会命中该分支）。
async fn insert_row_on(
    conn: &mut sqlx::SqliteConnection,
    uid: i64,
    token_version: i64,
    family_id: &str,
    ttl_secs: u64,
) -> Result<Option<String>, AppError> {
    let token = generate_token();
    let token_hash = hash_token(&token);
    let now = now_ts();
    sqlx::query(
        "INSERT INTO refresh_tokens \
         (token_hash, uid, token_version, family_id, issued_at, expires_at, revoked_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
                 (SELECT MAX(revoked_at) FROM refresh_tokens WHERE family_id=?4))",
    )
    .bind(&token_hash)
    .bind(uid)
    .bind(token_version)
    .bind(family_id)
    .bind(now)
    .bind(now.saturating_add(ttl_secs as i64))
    .execute(&mut *conn)
    .await?;

    let revoked: Option<i64> =
        sqlx::query("SELECT revoked_at FROM refresh_tokens WHERE token_hash=?1")
            .bind(&token_hash)
            .fetch_one(&mut *conn)
            .await?
            .get("revoked_at");
    Ok(if revoked.is_some() { None } else { Some(token) })
}

fn is_well_formed(raw: &str) -> bool {
    raw.starts_with(TOKEN_PREFIX) && raw.len() > TOKEN_PREFIX.len() && raw.len() <= MAX_TOKEN_LEN
}

fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", hex::encode(bytes))
}

fn hash_token(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use crate::util::crypto::random_string;
    use std::path::PathBuf;
    use tokio::fs;

    async fn store_with_ttl(ttl_secs: u64) -> (RefreshTokenStore, SqlitePool, PathBuf) {
        crate::util::test_temp::clean_stale_test_dirs();
        let dir = std::env::temp_dir().join(format!("reader-rust-refresh-{}", random_string(8)));
        std::fs::create_dir_all(&dir).unwrap();
        let database_url = format!("sqlite:{}?mode=rwc", dir.join("reader.db").display());
        let pool = db::init_pool(&database_url).await.unwrap();
        sqlx::query(
            "INSERT INTO users (uid, username, password_hash, token_version, created_at, last_login_at) \
             VALUES (1, 'reader1', 'placeholder', 0, 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        (RefreshTokenStore::new(pool.clone(), ttl_secs), pool, dir)
    }

    async fn finish(pool: SqlitePool, dir: PathBuf) {
        pool.close().await;
        let _ = fs::remove_dir_all(dir).await;
    }

    fn rotated(outcome: RotateOutcome) -> RefreshedSession {
        match outcome {
            RotateOutcome::Rotated(session) => session,
            other => panic!("应轮换成功，实际: {other:?}"),
        }
    }

    #[tokio::test]
    async fn issued_token_is_random_and_only_hash_is_persisted() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let token = store.issue(1, 0).await.unwrap();

        assert!(token.starts_with("rt_"));
        assert_eq!(token.len(), TOKEN_PREFIX.len() + TOKEN_BYTES * 2);
        assert_ne!(token, store.issue(1, 0).await.unwrap());

        let row = sqlx::query("SELECT token_hash FROM refresh_tokens WHERE token_hash=?1")
            .bind(hash_token(&token))
            .fetch_optional(&pool)
            .await
            .unwrap()
            .expect("令牌摘要应已落库");
        let stored: String = row.get("token_hash");
        assert_eq!(stored, hash_token(&token));
        assert!(!stored.starts_with("rt_"), "库里不能出现令牌明文");

        finish(pool, dir).await;
    }

    #[tokio::test]
    async fn rotation_consumes_old_token_and_returns_a_new_one() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let first = store.issue(1, 0).await.unwrap();
        let second = rotated(store.rotate(&first).await.unwrap());
        assert_ne!(second.refresh_token, first);
        assert_eq!(second.uid, 1);
        assert_eq!(second.username, "reader1");

        let used: Option<i64> =
            sqlx::query("SELECT used_at FROM refresh_tokens WHERE token_hash=?1")
                .bind(hash_token(&first))
                .fetch_one(&pool)
                .await
                .unwrap()
                .get("used_at");
        assert!(used.is_some(), "旧令牌必须被标记为已消费");

        let third = rotated(store.rotate(&second.refresh_token).await.unwrap());
        assert_ne!(third.refresh_token, second.refresh_token);
        finish(pool, dir).await;
    }

    #[tokio::test]
    async fn reuse_inside_grace_window_is_tolerated() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let first = store.issue(1, 0).await.unwrap();
        let second = rotated(store.rotate(&first).await.unwrap());

        // 断网丢响应后的立即重试：宽限窗口内应再发一枚，而不是吊销家族
        let retry = rotated(store.rotate(&first).await.unwrap());
        assert_ne!(retry.refresh_token, second.refresh_token);
        // 第一次轮换发出的令牌仍然有效
        rotated(store.rotate(&second.refresh_token).await.unwrap());
        finish(pool, dir).await;
    }

    #[tokio::test]
    async fn reuse_after_grace_revokes_the_whole_family() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let first = store.issue(1, 0).await.unwrap();
        let second = rotated(store.rotate(&first).await.unwrap());

        // 把消费时间回拨到宽限窗口之外
        sqlx::query("UPDATE refresh_tokens SET used_at=?1 WHERE token_hash=?2")
            .bind(now_ts() - REUSE_GRACE_SECS - 10)
            .bind(hash_token(&first))
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            store.rotate(&first).await.unwrap(),
            RotateOutcome::ReuseDetected
        );
        // 家族里其余令牌一并失效
        assert_eq!(
            store.rotate(&second.refresh_token).await.unwrap(),
            RotateOutcome::Invalid
        );
        finish(pool, dir).await;
    }

    #[tokio::test]
    async fn expired_or_malformed_tokens_are_invalid() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let token = store.issue(1, 0).await.unwrap();
        sqlx::query("UPDATE refresh_tokens SET expires_at=?1")
            .bind(now_ts() - 1)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(store.rotate(&token).await.unwrap(), RotateOutcome::Invalid);

        for bad in ["", "jwt.lookalike.value", "rt_", "rt_short"] {
            assert_eq!(store.rotate(bad).await.unwrap(), RotateOutcome::Invalid);
        }
        finish(pool, dir).await;
    }

    #[tokio::test]
    async fn version_mismatch_invalidates_and_revokes_family() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let token = store.issue(1, 0).await.unwrap();
        sqlx::query("UPDATE users SET token_version=1 WHERE uid=1")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(store.rotate(&token).await.unwrap(), RotateOutcome::Invalid);
        let revoked: Option<i64> = sqlx::query("SELECT revoked_at FROM refresh_tokens LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("revoked_at");
        assert!(revoked.is_some(), "版本不匹配应顺手吊销家族");
        finish(pool, dir).await;
    }

    #[tokio::test]
    async fn new_rows_inherit_family_revocation() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let first = store.issue(1, 0).await.unwrap();
        let second = rotated(store.rotate(&first).await.unwrap());
        let family_id: String =
            sqlx::query("SELECT family_id FROM refresh_tokens WHERE token_hash=?1")
                .bind(hash_token(&second.refresh_token))
                .fetch_one(&pool)
                .await
                .unwrap()
                .get("family_id");

        // 并发吊销发生在「读到未吊销」与「插入新行」之间：新行必须继承
        // 家族的 revoked_at，不能重新长出可用令牌
        sqlx::query("UPDATE refresh_tokens SET revoked_at=?1 WHERE family_id=?2")
            .bind(now_ts())
            .bind(&family_id)
            .execute(&pool)
            .await
            .unwrap();

        assert!(store.insert_row(1, 0, &family_id).await.unwrap().is_none());
        let revoked: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM refresh_tokens WHERE family_id=?1 AND revoked_at IS NOT NULL",
        )
        .bind(&family_id)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("n");
        assert_eq!(revoked, 3, "旧两行 + 新插入行都应带吊销时间");
        finish(pool, dir).await;
    }

    #[tokio::test]
    async fn revoke_all_for_user_kills_every_family() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let a = store.issue(1, 0).await.unwrap();
        let b = store.issue(1, 0).await.unwrap();

        assert_eq!(store.revoke_all_for_user(1).await.unwrap(), 2);
        assert_eq!(store.rotate(&a).await.unwrap(), RotateOutcome::Invalid);
        assert_eq!(store.rotate(&b).await.unwrap(), RotateOutcome::Invalid);
        finish(pool, dir).await;
    }

    #[tokio::test]
    async fn logout_revokes_only_the_matching_family() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let device_a = store.issue(1, 0).await.unwrap();
        let device_b = store.issue(1, 0).await.unwrap();

        assert!(store.revoke_family_by_token(&device_a).await.unwrap());
        assert_eq!(
            store.rotate(&device_a).await.unwrap(),
            RotateOutcome::Invalid
        );
        // 另一台设备的会话不受影响
        rotated(store.rotate(&device_b).await.unwrap());
        assert!(!store.revoke_family_by_token("rt_unknown").await.unwrap());
        finish(pool, dir).await;
    }

    /// 吊销与轮换并发：无论谁先拿到写锁，家族里都不能再长出可用令牌。
    ///
    /// 轮换全程在 BEGIN IMMEDIATE 事务内，与吊销串行化——两种次序都收敛到
    /// 同一个不变量：轮换要么直接 Invalid（吊销先提交），要么发出的新令牌
    /// 随后被吊销一并失效（轮换先提交）。
    #[tokio::test]
    async fn concurrent_logout_and_rotate_never_leak_a_live_token() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let token = store.issue(1, 0).await.unwrap();

        let (revoke_result, rotate_result) =
            tokio::join!(store.revoke_family_by_token(&token), store.rotate(&token),);
        assert!(revoke_result.unwrap(), "登出吊销本身应成功");

        match rotate_result.unwrap() {
            RotateOutcome::Rotated(session) => {
                // 轮换先提交：随后的登出必须把新令牌一并吊销
                assert_eq!(
                    store.rotate(&session.refresh_token).await.unwrap(),
                    RotateOutcome::Invalid,
                    "吊销提交后，轮换出的新令牌也必须失效"
                );
            }
            RotateOutcome::Invalid | RotateOutcome::ReuseDetected => {}
        }
        assert_eq!(
            store.rotate(&token).await.unwrap(),
            RotateOutcome::Invalid,
            "已吊销家族的旧令牌必须 Invalid"
        );
        finish(pool, dir).await;
    }

    #[tokio::test]
    async fn purge_expired_removes_only_expired_rows() {
        let (store, pool, dir) = store_with_ttl(3600).await;
        let expired = store.issue(1, 0).await.unwrap();
        let active = store.issue(1, 0).await.unwrap();
        sqlx::query("UPDATE refresh_tokens SET expires_at=?1 WHERE token_hash=?2")
            .bind(now_ts() - 1)
            .bind(hash_token(&expired))
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(store.purge_expired().await.unwrap(), 1);
        assert_eq!(
            store.rotate(&expired).await.unwrap(),
            RotateOutcome::Invalid
        );
        rotated(store.rotate(&active).await.unwrap());
        finish(pool, dir).await;
    }
}
