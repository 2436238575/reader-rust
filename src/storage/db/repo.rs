use crate::error::error::AppError;
use crate::model::book_source::BookSource;
use crate::util::time::now_ts;
use sqlx::{Row, SqlitePool};

/// bookId 前缀：自带命名空间，避免与缓存目录里的 md5-hex 撞名，日志里可辨。
pub const BOOK_ID_PREFIX: &str = "book_";

/// books 登记表的一行。
#[derive(Debug, Clone)]
pub struct BookRow {
    pub book_id: String,
    pub book_url: String,
    pub book_source_id: Option<i64>,
    pub toc_url: Option<String>,
    pub name: String,
    pub author: String,
}

/// 批量登记的一条输入。
#[derive(Debug, Clone, Copy)]
pub struct RegisterBook<'a> {
    pub book_url: &'a str,
    pub book_source_id: Option<i64>,
    pub name: Option<&'a str>,
    pub author: Option<&'a str>,
    pub toc_url: Option<&'a str>,
}

#[derive(Clone)]
pub struct BookRepo {
    pool: SqlitePool,
}

fn map_book_row(row: &sqlx::sqlite::SqliteRow) -> BookRow {
    BookRow {
        book_id: row.get::<String, _>("book_id"),
        book_url: row.get::<String, _>("book_url"),
        book_source_id: row.get::<Option<i64>, _>("book_source_id"),
        toc_url: row.get::<Option<String>, _>("toc_url"),
        name: row.get::<String, _>("name"),
        author: row.get::<String, _>("author"),
    }
}

const BOOK_ROW_COLS: &str = "book_id, book_url, book_source_id, toc_url, name, author";

impl BookRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 分配下一个 bookId（per user_ns）。SQLite 单写者，MAX+1 安全。
    async fn next_id_on<'e, E>(executor: E, user_ns: &str) -> Result<String, AppError>
    where
        E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
    {
        let max: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(CAST(SUBSTR(book_id, 6) AS INTEGER)) FROM books \
             WHERE user_ns = ?1 AND book_id LIKE 'book_%'",
        )
        .bind(user_ns)
        .fetch_one(executor)
        .await?;
        Ok(format!("{BOOK_ID_PREFIX}{}", max.unwrap_or(0) + 1))
    }

    /// 登记单本书：URL 已存在 → 复用其 id 并补上可变字段；否则分配新 id。
    /// `desired_id` 仅供 WebDAV 恢复：空位才用，被占用（且 URL 不同）则分配新 id。
    pub async fn register(
        &self,
        user_ns: &str,
        item: RegisterBook<'_>,
        desired_id: Option<&str>,
    ) -> Result<String, AppError> {
        if let Some(existing) = self.get_by_url(user_ns, item.book_url).await? {
            touch_book_fields(&self.pool, user_ns, &existing.book_id, item).await?;
            return Ok(existing.book_id);
        }
        if let Some(desired) = desired_id {
            if let Some(existing) = self.get(user_ns, desired).await? {
                if existing.book_url == item.book_url {
                    touch_book_fields(&self.pool, user_ns, desired, item).await?;
                    return Ok(desired.to_string());
                }
                // id 被别的书占用：本地身份优先，新来的一律分配新 id
            } else {
                insert_book(&self.pool, user_ns, desired, item).await?;
                return Ok(desired.to_string());
            }
        }
        let id = Self::next_id_on(&self.pool, user_ns).await?;
        insert_book(&self.pool, user_ns, &id, item).await?;
        Ok(id)
    }

    /// 批量登记（搜索/发现结果），单事务。返回 bookUrl → bookId 映射。
    pub async fn register_many(
        &self,
        user_ns: &str,
        items: &[RegisterBook<'_>],
    ) -> Result<std::collections::HashMap<String, String>, AppError> {
        let mut tx = self.pool.begin().await?;
        let mut out = std::collections::HashMap::with_capacity(items.len());
        for item in items {
            let existing: Option<String> =
                sqlx::query_scalar("SELECT book_id FROM books WHERE user_ns=?1 AND book_url=?2")
                    .bind(user_ns)
                    .bind(item.book_url)
                    .fetch_optional(&mut *tx)
                    .await?;
            let id = match existing {
                Some(id) => {
                    touch_book_fields(&mut *tx, user_ns, &id, *item).await?;
                    id
                }
                None => {
                    let id = Self::next_id_on(&mut *tx, user_ns).await?;
                    insert_book(&mut *tx, user_ns, &id, *item).await?;
                    id
                }
            };
            out.insert(item.book_url.to_string(), id);
        }
        tx.commit().await?;
        Ok(out)
    }

    pub async fn get(&self, user_ns: &str, book_id: &str) -> Result<Option<BookRow>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {BOOK_ROW_COLS} FROM books WHERE user_ns=?1 AND book_id=?2"
        ))
        .bind(user_ns)
        .bind(book_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(map_book_row))
    }

    pub async fn get_by_url(
        &self,
        user_ns: &str,
        book_url: &str,
    ) -> Result<Option<BookRow>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {BOOK_ROW_COLS} FROM books WHERE user_ns=?1 AND book_url=?2"
        ))
        .bind(user_ns)
        .bind(book_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(map_book_row))
    }

    /// 换源改绑：book_id 不变，指向新的上游地址与书源。
    /// `book_url` 唯一冲突由调用方先处理（删孤儿行或对「目标已在架」报错）。
    pub async fn rebind(
        &self,
        user_ns: &str,
        book_id: &str,
        new_book_url: &str,
        new_source_id: Option<i64>,
    ) -> Result<(), AppError> {
        sqlx::query(
            "UPDATE books SET book_url=?3, book_source_id=?4, toc_url=NULL, updated_at=?5 \
             WHERE user_ns=?1 AND book_id=?2",
        )
        .bind(user_ns)
        .bind(book_id)
        .bind(new_book_url)
        .bind(new_source_id)
        .bind(now_ts())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete(&self, user_ns: &str, book_id: &str) -> Result<(), AppError> {
        sqlx::query("DELETE FROM books WHERE user_ns=?1 AND book_id=?2")
            .bind(user_ns)
            .bind(book_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// WebDAV 恢复的清表重建：恢复语义是全量覆盖，登记表随书架一起重置。
    pub async fn wipe(&self, user_ns: &str) -> Result<(), AppError> {
        sqlx::query("DELETE FROM books WHERE user_ns=?1")
            .bind(user_ns)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 回收不在给定保留清单里的登记行（purgeCache 清用户缓存时联动调用）。
    pub async fn delete_except(&self, user_ns: &str, keep_ids: &[String]) -> Result<u64, AppError> {
        if keep_ids.is_empty() {
            let result = sqlx::query("DELETE FROM books WHERE user_ns=?1")
                .bind(user_ns)
                .execute(&self.pool)
                .await?;
            return Ok(result.rows_affected());
        }
        let placeholders = keep_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM books WHERE user_ns=?1 AND book_id NOT IN ({placeholders})");
        let mut query = sqlx::query(&sql).bind(user_ns);
        for id in keep_ids {
            query = query.bind(id);
        }
        let result = query.execute(&self.pool).await?;
        Ok(result.rows_affected())
    }
}

/// 更新登记行的可变字段：只覆盖非空入参（登记时可能还不知道书名/目录）。
async fn touch_book_fields<'e, E>(
    executor: E,
    user_ns: &str,
    book_id: &str,
    item: RegisterBook<'_>,
) -> Result<(), AppError>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(
        "UPDATE books SET \
           book_source_id = COALESCE(?3, book_source_id), \
           toc_url = COALESCE(?4, toc_url), \
           name = CASE WHEN ?5 != '' THEN ?5 ELSE name END, \
           author = CASE WHEN ?6 != '' THEN ?6 ELSE author END, \
           updated_at = ?7 \
         WHERE user_ns = ?1 AND book_id = ?2",
    )
    .bind(user_ns)
    .bind(book_id)
    .bind(item.book_source_id)
    .bind(item.toc_url)
    .bind(item.name.unwrap_or(""))
    .bind(item.author.unwrap_or(""))
    .bind(now_ts())
    .execute(executor)
    .await?;
    Ok(())
}

async fn insert_book<'e, E>(
    executor: E,
    user_ns: &str,
    book_id: &str,
    item: RegisterBook<'_>,
) -> Result<(), AppError>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let now = now_ts();
    sqlx::query(
        "INSERT INTO books (user_ns, book_id, book_url, book_source_id, toc_url, name, author, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
    )
    .bind(user_ns)
    .bind(book_id)
    .bind(item.book_url)
    .bind(item.book_source_id)
    .bind(item.toc_url)
    .bind(item.name.unwrap_or(""))
    .bind(item.author.unwrap_or(""))
    .bind(now)
    .execute(executor)
    .await?;
    Ok(())
}
#[derive(Clone)]
pub struct BookSourceRepo {
    pool: SqlitePool,
}

impl BookSourceRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 分配下一个书源 id（per user_ns）。SQLite 单写者 + 调用方持事务，MAX+1 安全。
    async fn next_id_on<'e, E>(executor: E, user_ns: &str) -> Result<i64, AppError>
    where
        E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
    {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(book_source_id) FROM book_sources WHERE user_ns = ?1")
                .bind(user_ns)
                .fetch_one(executor)
                .await?;
        Ok(max.unwrap_or(0) + 1)
    }

    /// 按 URL upsert：新行自动分配 id；既有行（ON CONFLICT URL）保留原 id。
    /// 返回该书源的 bookSourceId。
    pub async fn upsert(
        &self,
        user_ns: &str,
        source: &BookSource,
        json: &str,
    ) -> Result<i64, AppError> {
        sqlx::query(
            "INSERT INTO book_sources (user_ns, book_source_url, book_source_name, json, updated_at, book_source_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, (SELECT COALESCE(MAX(book_source_id), 0) + 1 FROM book_sources WHERE user_ns = ?1)) \
             ON CONFLICT(user_ns, book_source_url) DO UPDATE SET book_source_name=excluded.book_source_name, json=excluded.json, updated_at=excluded.updated_at"
        )
        .bind(user_ns)
        .bind(&source.book_source_url)
        .bind(&source.book_source_name)
        .bind(json)
        .bind(now_ts())
        .execute(&self.pool)
        .await?;
        let id: i64 = sqlx::query_scalar(
            "SELECT book_source_id FROM book_sources WHERE user_ns=?1 AND book_source_url=?2",
        )
        .bind(user_ns)
        .bind(&source.book_source_url)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// 编辑器单条保存：按 id 更新（允许改 URL；URL 撞其他行报唯一约束）。
    /// 返回 false 表示该 id 不存在（调用方回落按 URL upsert）。
    pub async fn update_by_id(
        &self,
        user_ns: &str,
        id: i64,
        source: &BookSource,
        json: &str,
    ) -> Result<bool, AppError> {
        let result = sqlx::query(
            "UPDATE book_sources SET book_source_url=?3, book_source_name=?4, json=?5, updated_at=?6 \
             WHERE user_ns=?1 AND book_source_id=?2",
        )
        .bind(user_ns)
        .bind(id)
        .bind(&source.book_source_url)
        .bind(&source.book_source_name)
        .bind(json)
        .bind(now_ts())
        .execute(&self.pool)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
                AppError::BadRequest("bookSourceUrl 已被其他书源占用".to_string())
            }
            _ => AppError::Internal(e.into()),
        })?;
        Ok(result.rows_affected() > 0)
    }

    /// 批量导入/恢复：URL 去重优先——URL 已存在则更新内容、保本地 id；
    /// URL 不存在时优先使用自带 id（空位才用，被占用则分配新 id）。
    /// 单事务提交：逐条独立事务时导入上千书源 = 上千次 fsync，且失去原子性。
    pub async fn import_many(
        &self,
        user_ns: &str,
        entries: &[(BookSource, String, Option<i64>)],
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        for (source, json, desired_id) in entries {
            let existing: Option<i64> = sqlx::query_scalar(
                "SELECT book_source_id FROM book_sources WHERE user_ns=?1 AND book_source_url=?2",
            )
            .bind(user_ns)
            .bind(&source.book_source_url)
            .fetch_optional(&mut *tx)
            .await?;
            if existing.is_some() {
                sqlx::query(
                    "UPDATE book_sources SET book_source_name=?3, json=?4, updated_at=?5 \
                     WHERE user_ns=?1 AND book_source_url=?2",
                )
                .bind(user_ns)
                .bind(&source.book_source_url)
                .bind(&source.book_source_name)
                .bind(json)
                .bind(now_ts())
                .execute(&mut *tx)
                .await?;
                continue;
            }
            let id = match desired_id {
                Some(d) => {
                    let taken: Option<i64> = sqlx::query_scalar(
                        "SELECT book_source_id FROM book_sources WHERE user_ns=?1 AND book_source_id=?2",
                    )
                    .bind(user_ns)
                    .bind(*d)
                    .fetch_optional(&mut *tx)
                    .await?;
                    if taken.is_some() {
                        Self::next_id_on(&mut *tx, user_ns).await?
                    } else {
                        *d
                    }
                }
                None => Self::next_id_on(&mut *tx, user_ns).await?,
            };
            sqlx::query(
                "INSERT INTO book_sources (user_ns, book_source_url, book_source_name, json, updated_at, book_source_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(user_ns)
            .bind(&source.book_source_url)
            .bind(&source.book_source_name)
            .bind(json)
            .bind(now_ts())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_by_id(&self, user_ns: &str, id: i64) -> Result<(), AppError> {
        sqlx::query("DELETE FROM book_sources WHERE user_ns=?1 AND book_source_id=?2")
            .bind(user_ns)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_all(&self, user_ns: &str) -> Result<(), AppError> {
        sqlx::query("DELETE FROM book_sources WHERE user_ns=?1")
            .bind(user_ns)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_by_id(
        &self,
        user_ns: &str,
        id: i64,
    ) -> Result<Option<(String, String)>, AppError> {
        let row = sqlx::query(
            "SELECT book_source_url, json FROM book_sources WHERE user_ns=?1 AND book_source_id=?2",
        )
        .bind(user_ns)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            (
                r.get::<String, _>("book_source_url"),
                r.get::<String, _>("json"),
            )
        }))
    }

    /// 列出全部书源：`(id, json)`，保持原有 updated_at 倒序。
    pub async fn list(&self, user_ns: &str) -> Result<Vec<(i64, String)>, AppError> {
        let rows = sqlx::query(
            "SELECT book_source_id, json FROM book_sources WHERE user_ns=?1 ORDER BY updated_at DESC",
        )
        .bind(user_ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<i64, _>("book_source_id"),
                    r.get::<String, _>("json"),
                )
            })
            .collect())
    }
}
