//! 阅读统计明细表与旧统计总额的数据访问。
//!
//! 写路径全部使用 `BEGIN IMMEDIATE`：阅读事件是并发的短事务，WAL 下
//! 串行化写者，避免 DEFERRED 升级写锁时的 SQLITE_BUSY_SNAPSHOT。

use std::collections::HashSet;

use sqlx::{Row, SqlitePool};

#[derive(Clone)]
pub struct ReadStatsRepo {
    pool: SqlitePool,
}

/// 一条待写入的阅读事件：`(book_id, chapter_index, seconds)`。
pub type ReadEventRow = (i64, i32, i64);

impl ReadStatsRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 全部账号的命名空间（`user_<uid>`）。启动回填按账号逐个执行。
    pub async fn user_namespaces(&self) -> Result<Vec<String>, sqlx::Error> {
        let rows = sqlx::query("SELECT uid FROM users ORDER BY uid")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| crate::auth::middleware::user_ns_of(row.get("uid")))
            .collect())
    }

    /// 该用户是否已经有新表数据（回填幂等判据）。
    pub async fn has_rows(&self, user_ns: &str) -> Result<bool, sqlx::Error> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM read_chapter_days WHERE user_ns=?1")
                .bind(user_ns)
                .fetch_one(&self.pool)
                .await?;
        Ok(count > 0)
    }

    /// 已完成回填的时间戳；`None` 表示还没回填过。
    pub async fn backfilled_at(&self, user_ns: &str) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar("SELECT backfilled_at FROM read_stats_legacy WHERE user_ns=?1")
            .bind(user_ns)
            .fetch_optional(&self.pool)
            .await
    }

    /// 批量确认 bookId 是否都在 books 登记表里。返回命中的数字 id。
    pub async fn registered_book_ids(&self, ids: &[i64]) -> Result<HashSet<i64>, sqlx::Error> {
        let mut found = HashSet::with_capacity(ids.len());
        if ids.is_empty() {
            return Ok(found);
        }
        let placeholders = (1..=ids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT book_id FROM books WHERE book_id IN ({placeholders})");
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for id in ids {
            query = query.bind(id);
        }
        for row in query.fetch_all(&self.pool).await? {
            found.insert(row.get::<i64, _>("book_id"));
        }
        Ok(found)
    }

    /// 同一事务内：逐条 upsert 明细并推进该书 `shelf_books.dur_chapter_time`。
    ///
    /// `seconds = MAX(旧值, 新值)` 让客户端可以按章重复上报当日累计秒数；
    /// 真实上报会把回填行的 `backfilled` 清 0（前提是本次有正秒数）。
    pub async fn upsert_events(
        &self,
        user_ns: &str,
        day_key: i64,
        events: &[ReadEventRow],
        now: i64,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        for (book_id, chapter_index, seconds) in events {
            sqlx::query(
                "INSERT INTO read_chapter_days \
                     (user_ns, book_id, chapter_index, day_key, seconds, updated_at, backfilled) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0) \
                 ON CONFLICT(user_ns, book_id, chapter_index, day_key) DO UPDATE SET \
                     seconds = MAX(read_chapter_days.seconds, excluded.seconds), \
                     updated_at = excluded.updated_at, \
                     backfilled = CASE WHEN excluded.seconds > 0 THEN 0 \
                                       ELSE read_chapter_days.backfilled END",
            )
            .bind(user_ns)
            .bind(book_id)
            .bind(chapter_index)
            .bind(day_key)
            .bind(seconds)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        let mut books: Vec<i64> = events.iter().map(|(book_id, _, _)| *book_id).collect();
        books.sort_unstable();
        books.dedup();
        for book_id in books {
            sqlx::query(
                "UPDATE shelf_books \
                 SET dur_chapter_time = MAX(COALESCE(dur_chapter_time, 0), ?3), updated_at=?3 \
                 WHERE user_ns=?1 AND book_id=?2",
            )
            .bind(user_ns)
            .bind(book_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    /// 历史回填：所有明细落在同一个 `day_key` 且 `backfilled=1`（秒数 0），
    /// 同时写入旧版总时长与回填完成标记。整批一个事务。
    pub async fn insert_backfill(
        &self,
        user_ns: &str,
        day_key: i64,
        rows: &[(i64, i32)],
        legacy_total_seconds: i64,
        now: i64,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        for (book_id, chapter_index) in rows {
            sqlx::query(
                "INSERT OR IGNORE INTO read_chapter_days \
                     (user_ns, book_id, chapter_index, day_key, seconds, updated_at, backfilled) \
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, 1)",
            )
            .bind(user_ns)
            .bind(book_id)
            .bind(chapter_index)
            .bind(day_key)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "INSERT INTO read_stats_legacy (user_ns, legacy_total_seconds, backfilled_at) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(user_ns) DO UPDATE SET \
                 legacy_total_seconds = MAX(read_stats_legacy.legacy_total_seconds, excluded.legacy_total_seconds), \
                 backfilled_at = excluded.backfilled_at",
        )
        .bind(user_ns)
        .bind(legacy_total_seconds.max(0))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }

    async fn legacy_total_seconds(&self, user_ns: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COALESCE(MAX(legacy_total_seconds), 0) FROM read_stats_legacy WHERE user_ns=?1",
        )
        .bind(user_ns)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn total_seconds(&self, user_ns: &str) -> Result<i64, sqlx::Error> {
        let detail: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(seconds), 0) FROM read_chapter_days WHERE user_ns=?1",
        )
        .bind(user_ns)
        .fetch_one(&self.pool)
        .await?;
        Ok(detail.saturating_add(self.legacy_total_seconds(user_ns).await?))
    }

    pub async fn today_seconds(&self, user_ns: &str, day_key: i64) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(seconds), 0) FROM read_chapter_days \
             WHERE user_ns=?1 AND day_key=?2 AND backfilled=0",
        )
        .bind(user_ns)
        .bind(day_key)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn read_chapters(&self, user_ns: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM (\
                 SELECT DISTINCT book_id, chapter_index FROM read_chapter_days WHERE user_ns=?1\
             )",
        )
        .bind(user_ns)
        .fetch_one(&self.pool)
        .await
    }

    /// 书架内「最后一章已读」的本数。
    ///
    /// - 网络书：目录抓取完整（`book_toc_state.complete=1`）时取 `chapters`
    ///   最大 index；未完整但已知 `total_chapter_num` 时用 `total-1`，
    ///   避免把被分页上限截断的最后一章误判成全书最后一章；
    /// - 本地 TXT/EPUB 不写章节登记表，直接用 `total_chapter_num - 1`。
    pub async fn finished_books(&self, user_ns: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM (\
                 SELECT sb.book_id, \
                        CASE \
                            WHEN c.last_index IS NULL THEN COALESCE(sb.total_chapter_num, 0) - 1 \
                            WHEN COALESCE(ts.complete, 0) = 1 THEN c.last_index \
                            WHEN COALESCE(sb.total_chapter_num, 0) > 0 THEN sb.total_chapter_num - 1 \
                            ELSE -1 \
                        END AS last_index \
                 FROM shelf_books sb \
                 LEFT JOIN (SELECT book_id, MAX(chapter_index) AS last_index \
                            FROM chapters GROUP BY book_id) c ON c.book_id = sb.book_id \
                 LEFT JOIN book_toc_state ts ON ts.book_id = sb.book_id \
                 WHERE sb.user_ns = ?1\
             ) x \
             WHERE x.last_index >= 0 AND EXISTS (\
                 SELECT 1 FROM read_chapter_days r \
                 WHERE r.user_ns = ?1 AND r.book_id = x.book_id \
                   AND r.chapter_index = x.last_index\
             )",
        )
        .bind(user_ns)
        .fetch_one(&self.pool)
        .await
    }

    /// 有正阅读秒数、且不是纯回填的自然日（降序，用于连续天数）。
    pub async fn positive_days(&self, user_ns: &str) -> Result<Vec<i64>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT day_key FROM read_chapter_days \
             WHERE user_ns=?1 AND backfilled=0 AND seconds>0 \
             GROUP BY day_key ORDER BY day_key DESC",
        )
        .bind(user_ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|row| row.get("day_key")).collect())
    }
}
