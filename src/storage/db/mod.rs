pub mod repo;

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};
use std::str::FromStr;

pub async fn init_pool(database_url: &str) -> anyhow::Result<SqlitePool> {
    // WAL：读不阻塞写（默认 DELETE 模式下写事务会阻塞全部读）；
    // synchronous=NORMAL 是 WAL 的官方推荐档位（掉电只可能丢最后一个事务，不损坏库）。
    //
    // 写事务一律 BEGIN IMMEDIATE（见 repo.rs）：WAL 下它只在写者间串行，
    // 不阻塞并发读；而 DEFERRED 先读后写会在升级写锁时因快照过期直接
    // SQLITE_BUSY_SNAPSHOT（busy_timeout 管不到），表现为并发写偶发 500。
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        // 写锁等待 5s：WAL 下并发写者排队而不是立刻 SQLITE_BUSY
        .busy_timeout(std::time::Duration::from_secs(5))
        .synchronous(SqliteSynchronous::Normal);
    // 鉴权中间件与业务 handler 各占一条连接，批量缓存/搜索并发 24 起步，5 条容易排队。
    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await?;
    sqlx::migrate!("src/storage/db/migrations")
        .run(&pool)
        .await?;
    Ok(pool)
}
