use crate::error::error::AppError;
use crate::model::book_source::BookSource;
use crate::util::time::now_ts;
use sqlx::{Row, SqlitePool};

/// bookId 前缀：对外（API/缓存路径/日志）一律是 `book_<n>` 文本，自带命名空间
/// 避免与缓存目录里的 md5-hex 撞名；数据库列存 INTEGER（rowid 单调分配）。
pub const BOOK_ID_PREFIX: &str = "book_";

/// `book_<n>` → n。非法格式返回 None（调用方按「不存在」或参数错误处理）。
pub fn parse_book_id(s: &str) -> Option<i64> {
    s.strip_prefix(BOOK_ID_PREFIX)?.parse::<i64>().ok()
}

/// n → `book_<n>`。
pub fn format_book_id(id: i64) -> String {
    format!("{BOOK_ID_PREFIX}{id}")
}

/// books 登记表的一行。book_id 对外是格式化后的 `book_<n>` 文本。
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
        book_id: format_book_id(row.get::<i64, _>("book_id")),
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

    /// 登记单本书：URL 已存在 → 复用其 id 并补上可变字段；否则分配新 id
    ///（books.book_id 是 rowid 别名，不显式插入即单调自增）。
    /// `desired_id` 仅供 WebDAV 恢复：必须形如 `book_<n>`（否则 400），
    /// 空位才用，被占用（且 URL 不同）则分配新 id。
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
            let desired_num = parse_book_id(desired)
                .ok_or_else(|| AppError::BadRequest(format!("bookId 格式非法: {desired:?}")))?;
            if let Some(existing) = self.get(user_ns, desired).await? {
                if existing.book_url == item.book_url {
                    touch_book_fields(&self.pool, user_ns, desired, item).await?;
                    return Ok(desired.to_string());
                }
                // id 被别的书占用：本地身份优先，新来的一律分配新 id
            } else {
                insert_book_with_id(&self.pool, user_ns, desired_num, item).await?;
                return Ok(desired.to_string());
            }
        }
        insert_book_auto(&self.pool, user_ns, item)
            .await
            .map(|(id, _)| id)
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
            let existing: Option<i64> =
                sqlx::query_scalar("SELECT book_id FROM books WHERE user_ns=?1 AND book_url=?2")
                    .bind(user_ns)
                    .bind(item.book_url)
                    .fetch_optional(&mut *tx)
                    .await?;
            let id = match existing {
                Some(id) => {
                    touch_book_fields(&mut *tx, user_ns, &format_book_id(id), *item).await?;
                    id
                }
                None => insert_book_auto(&mut *tx, user_ns, *item).await?.1,
            };
            out.insert(item.book_url.to_string(), format_book_id(id));
        }
        tx.commit().await?;
        Ok(out)
    }

    /// 开事务：save_books 的整架替换/恢复需要跨 books+shelf_books 的原子性。
    pub async fn begin(&self) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>, AppError> {
        Ok(self.pool.begin().await?)
    }

    /// 事务版 wipe。
    pub async fn wipe_on(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_ns: &str,
    ) -> Result<(), AppError> {
        sqlx::query("DELETE FROM books WHERE user_ns=?1")
            .bind(user_ns)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// 事务版 register：与 `register` 语义一致，全部读写落在给定事务里。
    pub async fn register_on(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_ns: &str,
        item: RegisterBook<'_>,
        desired_id: Option<&str>,
    ) -> Result<String, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {BOOK_ROW_COLS} FROM books WHERE user_ns=?1 AND book_url=?2"
        ))
        .bind(user_ns)
        .bind(item.book_url)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(row) = row {
            let existing = map_book_row(&row);
            touch_book_fields(&mut **tx, user_ns, &existing.book_id, item).await?;
            return Ok(existing.book_id);
        }
        if let Some(desired) = desired_id {
            let desired_num = parse_book_id(desired)
                .ok_or_else(|| AppError::BadRequest(format!("bookId 格式非法: {desired:?}")))?;
            let row = sqlx::query(&format!(
                "SELECT {BOOK_ROW_COLS} FROM books WHERE user_ns=?1 AND book_id=?2"
            ))
            .bind(user_ns)
            .bind(desired_num)
            .fetch_optional(&mut **tx)
            .await?;
            match row {
                Some(row) if map_book_row(&row).book_url == item.book_url => {
                    touch_book_fields(&mut **tx, user_ns, desired, item).await?;
                    return Ok(desired.to_string());
                }
                // id 被别的书占用：本地身份优先，新来的一律分配新 id
                Some(_) => {}
                None => {
                    insert_book_with_id(&mut **tx, user_ns, desired_num, item).await?;
                    return Ok(desired.to_string());
                }
            }
        }
        insert_book_auto(&mut **tx, user_ns, item)
            .await
            .map(|(id, _)| id)
    }

    pub async fn get(&self, user_ns: &str, book_id: &str) -> Result<Option<BookRow>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(None);
        };
        let row = sqlx::query(&format!(
            "SELECT {BOOK_ROW_COLS} FROM books WHERE user_ns=?1 AND book_id=?2"
        ))
        .bind(user_ns)
        .bind(id)
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
        let Some(id) = parse_book_id(book_id) else {
            return Ok(());
        };
        sqlx::query(
            "UPDATE books SET book_url=?3, book_source_id=?4, toc_url=NULL, updated_at=?5 \
             WHERE user_ns=?1 AND book_id=?2",
        )
        .bind(user_ns)
        .bind(id)
        .bind(new_book_url)
        .bind(new_source_id)
        .bind(now_ts())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete(&self, user_ns: &str, book_id: &str) -> Result<(), AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(());
        };
        sqlx::query("DELETE FROM books WHERE user_ns=?1 AND book_id=?2")
            .bind(user_ns)
            .bind(id)
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
        let ids: Vec<i64> = keep_ids.iter().filter_map(|s| parse_book_id(s)).collect();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM books WHERE user_ns=? AND book_id NOT IN ({placeholders})");
        let mut query = sqlx::query(&sql).bind(user_ns);
        for id in ids {
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
    let Some(id) = parse_book_id(book_id) else {
        return Ok(());
    };
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
    .bind(id)
    .bind(item.book_source_id)
    .bind(item.toc_url)
    .bind(item.name.unwrap_or(""))
    .bind(item.author.unwrap_or(""))
    .bind(now_ts())
    .execute(executor)
    .await?;
    Ok(())
}

const BOOK_INSERT_COLS: &str =
    "user_ns, book_url, book_source_id, toc_url, name, author, created_at, updated_at";

fn bind_book_fields<'q>(
    query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    user_ns: &'q str,
    item: RegisterBook<'q>,
    now: i64,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    query
        .bind(user_ns)
        .bind(item.book_url)
        .bind(item.book_source_id)
        .bind(item.toc_url)
        .bind(item.name.unwrap_or(""))
        .bind(item.author.unwrap_or(""))
        .bind(now)
}

/// 不显式给 id 的插入：book_id 是 rowid 别名，由 SQLite 单调分配。
/// 返回 `(格式化 id, 数值 id)`。
async fn insert_book_auto<'e, E>(
    executor: E,
    user_ns: &str,
    item: RegisterBook<'_>,
) -> Result<(String, i64), AppError>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let now = now_ts();
    let sql =
        format!("INSERT INTO books ({BOOK_INSERT_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)");
    let result = bind_book_fields(sqlx::query(&sql), user_ns, item, now)
        .execute(executor)
        .await?;
    let id = result.last_insert_rowid();
    Ok((format_book_id(id), id))
}

/// 显式指定 id 的插入（WebDAV 恢复对齐备份）。
async fn insert_book_with_id<'e, E>(
    executor: E,
    user_ns: &str,
    id: i64,
    item: RegisterBook<'_>,
) -> Result<(), AppError>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let now = now_ts();
    let sql = format!(
        "INSERT INTO books (book_id, {BOOK_INSERT_COLS}) VALUES (?8, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)"
    );
    bind_book_fields(sqlx::query(&sql), user_ns, item, now)
        .bind(id)
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

// ─── 书架（shelf_books 全列展开） ─────────────────────────────────────────

/// shelf_books 的一行：书架状态与元数据快照。身份与绑定字段（bookUrl/origin/
/// tocUrl/name/author/bookSourceId）不在此表——读取时 JOIN books 注入。
#[derive(Debug, Clone, Default)]
pub struct ShelfBookRow {
    pub book_id: String,
    pub position: i64,
    pub group_bits: Option<i64>,
    pub dur_chapter_index: Option<i32>,
    pub dur_chapter_pos: Option<i32>,
    pub dur_chapter_time: Option<i64>,
    pub dur_chapter_title: Option<String>,
    pub cover_url: Option<String>,
    pub custom_cover_url: Option<String>,
    pub can_update: Option<bool>,
    pub intro: Option<String>,
    pub latest_chapter_title: Option<String>,
    pub last_check_time: Option<i64>,
    pub total_chapter_num: Option<i32>,
    pub book_type: Option<i32>,
    pub word_count: Option<String>,
    pub info_html: Option<String>,
    pub toc_html: Option<String>,
    pub kind: Option<String>,
    pub update_time: Option<String>,
    pub can_re_name: Option<String>,
    pub download_urls: Option<String>,
    pub charset: Option<String>,
}

/// JOIN books 后的完整书架条目：书架行 + 登记表行 + 书源（若有）。
#[derive(Debug, Clone)]
pub struct ShelfEntry {
    pub shelf: ShelfBookRow,
    pub book_url: String,
    pub book_source_id: Option<i64>,
    pub toc_url: Option<String>,
    pub name: String,
    pub author: String,
    pub origin_url: Option<String>,
    pub origin_name: Option<String>,
}

const SHELF_COLS: &str = "s.book_id, s.position, s.group_bits, s.dur_chapter_index, \
     s.dur_chapter_pos, s.dur_chapter_time, s.dur_chapter_title, s.cover_url, \
     s.custom_cover_url, s.can_update, s.intro, s.latest_chapter_title, \
     s.last_check_time, s.total_chapter_num, s.book_type, s.word_count, s.info_html, \
     s.toc_html, s.kind, s.update_time, s.can_re_name, s.download_urls, s.charset";

const SHELF_JOIN_COLS: &str = "b.book_url, b.book_source_id, b.toc_url, b.name, b.author, \
     bs.book_source_url AS origin_url, bs.book_source_name AS origin_name";

fn map_shelf_entry(row: &sqlx::sqlite::SqliteRow) -> ShelfEntry {
    ShelfEntry {
        shelf: ShelfBookRow {
            book_id: format_book_id(row.get::<i64, _>("book_id")),
            position: row.get::<i64, _>("position"),
            group_bits: row.get::<Option<i64>, _>("group_bits"),
            dur_chapter_index: row.get::<Option<i32>, _>("dur_chapter_index"),
            dur_chapter_pos: row.get::<Option<i32>, _>("dur_chapter_pos"),
            dur_chapter_time: row.get::<Option<i64>, _>("dur_chapter_time"),
            dur_chapter_title: row.get::<Option<String>, _>("dur_chapter_title"),
            cover_url: row.get::<Option<String>, _>("cover_url"),
            custom_cover_url: row.get::<Option<String>, _>("custom_cover_url"),
            can_update: row.get::<Option<i64>, _>("can_update").map(|v| v != 0),
            intro: row.get::<Option<String>, _>("intro"),
            latest_chapter_title: row.get::<Option<String>, _>("latest_chapter_title"),
            last_check_time: row.get::<Option<i64>, _>("last_check_time"),
            total_chapter_num: row.get::<Option<i32>, _>("total_chapter_num"),
            book_type: row.get::<Option<i32>, _>("book_type"),
            word_count: row.get::<Option<String>, _>("word_count"),
            info_html: row.get::<Option<String>, _>("info_html"),
            toc_html: row.get::<Option<String>, _>("toc_html"),
            kind: row.get::<Option<String>, _>("kind"),
            update_time: row.get::<Option<String>, _>("update_time"),
            can_re_name: row.get::<Option<String>, _>("can_re_name"),
            download_urls: row.get::<Option<String>, _>("download_urls"),
            charset: row.get::<Option<String>, _>("charset"),
        },
        book_url: row.get::<String, _>("book_url"),
        book_source_id: row.get::<Option<i64>, _>("book_source_id"),
        toc_url: row.get::<Option<String>, _>("toc_url"),
        name: row.get::<String, _>("name"),
        author: row.get::<String, _>("author"),
        origin_url: row.get::<Option<String>, _>("origin_url"),
        origin_name: row.get::<Option<String>, _>("origin_name"),
    }
}

#[derive(Clone)]
pub struct ShelfBookRepo {
    pool: SqlitePool,
}

impl ShelfBookRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 书架全量，按 position 排序。
    pub async fn list(&self, user_ns: &str) -> Result<Vec<ShelfEntry>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {SHELF_COLS}, {SHELF_JOIN_COLS} FROM shelf_books s \
             JOIN books b ON b.user_ns = s.user_ns AND b.book_id = s.book_id \
             LEFT JOIN book_sources bs ON bs.user_ns = s.user_ns AND bs.book_source_id = b.book_source_id \
             WHERE s.user_ns = ?1 ORDER BY s.position"
        ))
        .bind(user_ns)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(map_shelf_entry).collect())
    }

    pub async fn get(&self, user_ns: &str, book_id: &str) -> Result<Option<ShelfEntry>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(None);
        };
        let row = sqlx::query(&format!(
            "SELECT {SHELF_COLS}, {SHELF_JOIN_COLS} FROM shelf_books s \
             JOIN books b ON b.user_ns = s.user_ns AND b.book_id = s.book_id \
             LEFT JOIN book_sources bs ON bs.user_ns = s.user_ns AND bs.book_source_id = b.book_source_id \
             WHERE s.user_ns = ?1 AND s.book_id = ?2"
        ))
        .bind(user_ns)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(map_shelf_entry))
    }

    /// upsert 单条：position 为 None 时，既有行保留原位、新行排到末尾。
    pub async fn upsert(
        &self,
        user_ns: &str,
        mut row: ShelfBookRow,
        position: Option<i64>,
    ) -> Result<(), AppError> {
        let position = match position {
            Some(p) => p,
            None => {
                let id = parse_book_id(&row.book_id).ok_or_else(|| {
                    AppError::BadRequest(format!("bookId 格式非法: {:?}", row.book_id))
                })?;
                let existing: Option<i64> = sqlx::query_scalar(
                    "SELECT position FROM shelf_books WHERE user_ns=?1 AND book_id=?2",
                )
                .bind(user_ns)
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
                match existing {
                    Some(p) => p,
                    None => {
                        let max: Option<i64> = sqlx::query_scalar(
                            "SELECT MAX(position) FROM shelf_books WHERE user_ns=?1",
                        )
                        .bind(user_ns)
                        .fetch_one(&self.pool)
                        .await?;
                        max.unwrap_or(0) + 1
                    }
                }
            }
        };
        row.position = position;
        self.insert_row(&self.pool, user_ns, &row).await
    }

    /// 带限额的 upsert（saveBook 路径）：存在性检查、书架/本地书限额与写入
    /// 收进同一事务，消除 count 与 upsert 分离的 TOCTOU 窗口。
    /// `is_local` 由调用方按 book_url 前缀预判（本地书走 local-txt:/local-epub:）。
    pub async fn upsert_with_limit(
        &self,
        user_ns: &str,
        mut row: ShelfBookRow,
        shelf_limit: i64,
        local_limit: i64,
        is_local: bool,
    ) -> Result<(), AppError> {
        let book_id_num = parse_book_id(&row.book_id)
            .ok_or_else(|| AppError::BadRequest(format!("bookId 格式非法: {:?}", row.book_id)))?;
        let mut tx = self.pool.begin().await?;
        let existing_pos: Option<i64> =
            sqlx::query_scalar("SELECT position FROM shelf_books WHERE user_ns=?1 AND book_id=?2")
                .bind(user_ns)
                .bind(book_id_num)
                .fetch_optional(&mut *tx)
                .await?;
        if existing_pos.is_none() {
            // 限额只对「新入架」计数，已有书的更新不受限
            if shelf_limit > 0 {
                let count: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM shelf_books WHERE user_ns=?1")
                        .bind(user_ns)
                        .fetch_one(&mut *tx)
                        .await?;
                if count >= shelf_limit {
                    return Err(AppError::BadRequest(format!(
                        "书架书籍数量不能超过 {shelf_limit} 本"
                    )));
                }
            }
            if local_limit > 0 && is_local {
                let local_count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM shelf_books s JOIN books b \
                     ON b.user_ns = s.user_ns AND b.book_id = s.book_id \
                     WHERE s.user_ns=?1 AND (b.book_url LIKE 'local-txt:%' OR b.book_url LIKE 'local-epub:%')",
                )
                .bind(user_ns)
                .fetch_one(&mut *tx)
                .await?;
                if local_count >= local_limit {
                    return Err(AppError::BadRequest(format!(
                        "本地书籍数量不能超过 {local_limit} 本"
                    )));
                }
            }
        }
        row.position = match existing_pos {
            Some(p) => p,
            None => {
                let max: Option<i64> =
                    sqlx::query_scalar("SELECT MAX(position) FROM shelf_books WHERE user_ns=?1")
                        .bind(user_ns)
                        .fetch_one(&mut *tx)
                        .await?;
                max.unwrap_or(0) + 1
            }
        };
        self.insert_row(&mut *tx, user_ns, &row).await?;
        tx.commit().await?;
        Ok(())
    }

    /// 事务版整架替换（save_books 跨表原子性用）。
    pub async fn replace_all_on(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_ns: &str,
        rows: Vec<ShelfBookRow>,
    ) -> Result<(), AppError> {
        sqlx::query("DELETE FROM shelf_books WHERE user_ns=?1")
            .bind(user_ns)
            .execute(&mut **tx)
            .await?;
        for (index, mut row) in rows.into_iter().enumerate() {
            row.position = (index + 1) as i64;
            self.insert_row(&mut **tx, user_ns, &row).await?;
        }
        Ok(())
    }

    /// 事务版 wipe。
    pub async fn wipe_on(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        user_ns: &str,
    ) -> Result<(), AppError> {
        sqlx::query("DELETE FROM shelf_books WHERE user_ns=?1")
            .bind(user_ns)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// 单行写入（upsert 与 replace_all 共用）。
    async fn insert_row<'e, E>(
        &self,
        executor: E,
        user_ns: &str,
        row: &ShelfBookRow,
    ) -> Result<(), AppError>
    where
        E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
    {
        let book_id_num = parse_book_id(&row.book_id)
            .ok_or_else(|| AppError::BadRequest(format!("bookId 格式非法: {:?}", row.book_id)))?;
        let now = now_ts();
        sqlx::query(
            "INSERT INTO shelf_books (user_ns, book_id, position, group_bits, \
             dur_chapter_index, dur_chapter_pos, dur_chapter_time, dur_chapter_title, \
             cover_url, custom_cover_url, can_update, intro, latest_chapter_title, \
             last_check_time, total_chapter_num, book_type, word_count, info_html, toc_html, \
             kind, update_time, can_re_name, download_urls, charset, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
             ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26) \
             ON CONFLICT(user_ns, book_id) DO UPDATE SET position=excluded.position, \
             group_bits=excluded.group_bits, dur_chapter_index=excluded.dur_chapter_index, \
             dur_chapter_pos=excluded.dur_chapter_pos, dur_chapter_time=excluded.dur_chapter_time, \
             dur_chapter_title=excluded.dur_chapter_title, cover_url=excluded.cover_url, \
             custom_cover_url=excluded.custom_cover_url, can_update=excluded.can_update, \
             intro=excluded.intro, latest_chapter_title=excluded.latest_chapter_title, \
             last_check_time=excluded.last_check_time, total_chapter_num=excluded.total_chapter_num, \
             book_type=excluded.book_type, word_count=excluded.word_count, info_html=excluded.info_html, \
             toc_html=excluded.toc_html, kind=excluded.kind, update_time=excluded.update_time, \
             can_re_name=excluded.can_re_name, download_urls=excluded.download_urls, \
             charset=excluded.charset, updated_at=excluded.updated_at",
        )
        .bind(user_ns)
        .bind(book_id_num)
        .bind(row.position)
        .bind(row.group_bits)
        .bind(row.dur_chapter_index)
        .bind(row.dur_chapter_pos)
        .bind(row.dur_chapter_time)
        .bind(row.dur_chapter_title.as_deref())
        .bind(row.cover_url.as_deref())
        .bind(row.custom_cover_url.as_deref())
        .bind(row.can_update.map(|v| if v { 1i64 } else { 0i64 }))
        .bind(row.intro.as_deref())
        .bind(row.latest_chapter_title.as_deref())
        .bind(row.last_check_time)
        .bind(row.total_chapter_num)
        .bind(row.book_type)
        .bind(row.word_count.as_deref())
        .bind(row.info_html.as_deref())
        .bind(row.toc_html.as_deref())
        .bind(row.kind.as_deref())
        .bind(row.update_time.as_deref())
        .bind(row.can_re_name.as_deref())
        .bind(row.download_urls.as_deref())
        .bind(row.charset.as_deref())
        .bind(now)
        .bind(now)
        .execute(executor)
        .await?;
        Ok(())
    }

    pub async fn delete(&self, user_ns: &str, book_id: &str) -> Result<bool, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(false);
        };
        let result = sqlx::query("DELETE FROM shelf_books WHERE user_ns=?1 AND book_id=?2")
            .bind(user_ns)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_many(&self, user_ns: &str, book_ids: &[String]) -> Result<u64, AppError> {
        let mut deleted = 0u64;
        for id in book_ids {
            if self.delete(user_ns, id).await? {
                deleted += 1;
            }
        }
        Ok(deleted)
    }
}
