pub mod repo;

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};
use std::str::FromStr;

pub async fn init_pool(database_url: &str) -> anyhow::Result<SqlitePool> {
    // WAL：读不阻塞写（默认 DELETE 模式下写事务会阻塞全部读）；
    // synchronous=NORMAL 是 WAL 的官方推荐档位（掉电只可能丢最后一个事务，不损坏库）。
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal);
    // 鉴权中间件与业务 handler 各占一条连接，批量缓存/搜索并发 24 起步，5 条容易排队。
    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await?;
    sqlx::migrate!("src/storage/db/migrations")
        .run(&pool)
        .await?;
    backfill_book_source_ids(&pool).await?;
    backfill_user_uids(&pool).await?;
    Ok(pool)
}

/// 给 0004 迁移前已存在的用户行回填自增 uid。
/// 正常升级路径要求删除 storage/ 重建，这里只是防止旧库启动后 uid 全 NULL 的保险。
async fn backfill_user_uids(pool: &SqlitePool) -> anyhow::Result<()> {
    let rows = sqlx::query("SELECT rowid AS rid FROM users WHERE uid IS NULL ORDER BY rowid")
        .fetch_all(pool)
        .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    let mut next: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(uid), 0) + 1 FROM users")
        .fetch_one(&mut *tx)
        .await?;
    for row in &rows {
        sqlx::query("UPDATE users SET uid = ?1 WHERE rowid = ?2")
            .bind(next)
            .bind(row.get::<i64, _>("rid"))
            .execute(&mut *tx)
            .await?;
        next += 1;
    }
    tx.commit().await?;
    tracing::info!("已为 {} 个存量用户回填 uid", rows.len());
    Ok(())
}

/// 给 0002 迁移前已存在的书源行回填自增 id（新插入由 repo 层分配）。
/// 正常升级路径要求删除 storage/ 重建，这里只是防止旧库启动后 id 全 NULL 的保险。
async fn backfill_book_source_ids(pool: &SqlitePool) -> anyhow::Result<()> {
    let rows = sqlx::query(
        "SELECT rowid AS rid, user_ns FROM book_sources WHERE book_source_id IS NULL ORDER BY rowid",
    )
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    let mut next_per_ns: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for row in &rows {
        let ns = row.get::<String, _>("user_ns");
        let next = match next_per_ns.get(&ns) {
            Some(v) => *v,
            None => {
                let max: Option<i64> = sqlx::query_scalar(
                    "SELECT MAX(book_source_id) FROM book_sources WHERE user_ns = ?1",
                )
                .bind(&ns)
                .fetch_one(&mut *tx)
                .await?;
                max.unwrap_or(0) + 1
            }
        };
        sqlx::query("UPDATE book_sources SET book_source_id = ?1 WHERE rowid = ?2")
            .bind(next)
            .bind(row.get::<i64, _>("rid"))
            .execute(&mut *tx)
            .await?;
        next_per_ns.insert(ns, next + 1);
    }
    tx.commit().await?;
    tracing::info!("已为 {} 条存量书源回填 bookSourceId", rows.len());
    Ok(())
}
