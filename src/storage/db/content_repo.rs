//! 正文池、版本时间线和最新指针的数据访问。
//!
//! 三张表都不带用户命名空间：正文整部署共享。正文池和时间线只插入，
//! `chapter_latest` 是唯一允许更新的表；每用户的版本选择放在
//! `chapter_view_state`，仍然按用户隔离。

use crate::error::error::AppError;
use crate::storage::db::repo::parse_book_id;
use crate::util::time::now_millis;
use sqlx::{Row, SqlitePool};

/// `chapter_latest` 的一行。
#[derive(Debug, Clone, Default)]
pub struct ChapterLatestRow {
    pub latest_upstream_version_id: Option<i64>,
    pub latest_user_version_id: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub upstream_changed_at: Option<i64>,
    pub edited_at: Option<i64>,
    pub last_error: String,
}

/// `chapter_versions` 的一行（连带正文池的元数据，不含正文本体）。
#[derive(Debug, Clone)]
pub struct ChapterVersionRow {
    pub version_id: i64,
    pub content_id: i64,
    pub kind: String,
    pub book_source_id: Option<i64>,
    pub book_source_name: String,
    pub base_version_id: Option<i64>,
    pub chapter_index: i32,
    pub chapter_title: String,
    pub note: String,
    pub created_at: i64,
    pub raw_size: i64,
}

/// 新建一条版本时交给 repo 的全部字段。正文已经由 service 编码好。
pub struct NewChapterVersion<'a> {
    pub user_ns: &'a str,
    pub book_id: &'a str,
    pub chapter_url: &'a str,
    pub kind: &'a str,
    pub content_hash: &'a str,
    pub codec: i64,
    pub raw_size: i64,
    pub stored_size: i64,
    pub content: &'a [u8],
    pub book_source_id: Option<i64>,
    pub book_source_name: &'a str,
    pub base_version_id: Option<i64>,
    pub chapter_index: i32,
    pub chapter_title: &'a str,
    pub book_url: &'a str,
    pub fetch_url: &'a str,
    pub note: &'a str,
    /// 用户版本是否直接成为当前版本；上游版本忽略此字段。
    pub make_current: bool,
    /// 手动导入时可指定历史时间；None 用当前时间。只影响版本排序展示。
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum RecordOutcome {
    /// 内容与当前版本相同，没有追加时间线。
    Unchanged { version_id: i64 },
    /// 追加了一个新版本。
    Appended {
        version_id: i64,
        became_current: bool,
    },
}

/// 一本书里某用户当前会看到的一章（全文检索按章读取用）。
#[derive(Debug, Clone)]
pub struct EffectiveVersionRow {
    pub chapter_url: String,
    pub content_id: i64,
    pub kind: String,
    pub chapter_index: i32,
    pub chapter_title: String,
    pub raw_size: i64,
}

/// 当前显示版本的正文（仍是池里的编码形态），导入预览用于归一化比较。
#[derive(Debug, Clone)]
pub struct EffectiveContentBlob {
    pub chapter_url: String,
    pub codec: i64,
    pub content: Vec<u8>,
}

/// 一章的当前显示版本标记（阅读器目录标签用）。
#[derive(Debug, Clone)]
pub struct ChapterVersionMarkRow {
    pub chapter_url: String,
    pub kind: String,
    pub note: String,
    pub created_at: i64,
    pub is_latest_upstream: bool,
}

#[derive(Clone)]
pub struct ChapterContentRepo {
    pool: SqlitePool,
}

const VERSION_COLS: &str = "v.version_id, v.content_id, v.kind, \
     v.book_source_id, v.book_source_name, v.base_version_id, v.chapter_index, \
     v.chapter_title, v.note, v.created_at, c.raw_size";

fn map_latest(row: &sqlx::sqlite::SqliteRow) -> ChapterLatestRow {
    ChapterLatestRow {
        latest_upstream_version_id: row.get("latest_upstream_version_id"),
        latest_user_version_id: row.get("latest_user_version_id"),
        last_attempt_at: row.get("last_attempt_at"),
        last_success_at: row.get("last_success_at"),
        upstream_changed_at: row.get("upstream_changed_at"),
        edited_at: row.get("edited_at"),
        last_error: row.get("last_error"),
    }
}

fn map_version(row: &sqlx::sqlite::SqliteRow) -> ChapterVersionRow {
    ChapterVersionRow {
        version_id: row.get("version_id"),
        content_id: row.get("content_id"),
        kind: row.get("kind"),
        book_source_id: row.get("book_source_id"),
        book_source_name: row.get("book_source_name"),
        base_version_id: row.get("base_version_id"),
        chapter_index: row.get::<i64, _>("chapter_index") as i32,
        chapter_title: row.get("chapter_title"),
        note: row.get("note"),
        created_at: row.get("created_at"),
        raw_size: row.get("raw_size"),
    }
}

impl ChapterContentRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get_latest(
        &self,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<Option<ChapterLatestRow>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT book_id, chapter_url, latest_upstream_version_id, latest_user_version_id, \
             last_attempt_at, last_success_at, upstream_changed_at, edited_at, last_error \
             FROM chapter_latest WHERE book_id=?1 AND chapter_url=?2",
        )
        .bind(id)
        .bind(chapter_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(map_latest))
    }

    /// 解析某用户当前应显示的版本：
    /// 用户选择 > 最近用户版本 > 最近上游版本。`force_upstream` 只看上游预览。
    pub async fn resolve_version(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
        force_upstream: bool,
    ) -> Result<Option<ChapterVersionRow>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(None);
        };
        let sql = format!(
            "SELECT {VERSION_COLS}
             FROM chapter_latest l
             LEFT JOIN chapter_view_state s
               ON s.user_ns=?1 AND s.book_id=l.book_id AND s.chapter_url=l.chapter_url
             JOIN chapter_versions v
               ON v.version_id = CASE
                    WHEN ?4 THEN l.latest_upstream_version_id
                    ELSE COALESCE(
                      (SELECT sv.version_id FROM chapter_versions sv
                        WHERE sv.version_id = s.selected_version_id
                          AND sv.book_id = l.book_id AND sv.chapter_url = l.chapter_url),
                      (SELECT uv.version_id FROM chapter_versions uv
                        WHERE uv.version_id = l.latest_user_version_id
                          AND uv.book_id = l.book_id AND uv.chapter_url = l.chapter_url),
                      l.latest_upstream_version_id)
                    END
               AND v.book_id = l.book_id AND v.chapter_url = l.chapter_url
             JOIN chapter_contents c ON c.content_id = v.content_id
             WHERE l.book_id=?2 AND l.chapter_url=?3"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(user_ns)
            .bind(id)
            .bind(chapter_url)
            .bind(force_upstream)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(map_version))
    }

    pub async fn get_version(
        &self,
        book_id: &str,
        chapter_url: &str,
        version_id: i64,
    ) -> Result<Option<ChapterVersionRow>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(None);
        };
        let sql = format!(
            "SELECT {VERSION_COLS}
             FROM chapter_versions v
             JOIN chapter_contents c ON c.content_id = v.content_id
             WHERE v.version_id=?1 AND v.book_id=?2 AND v.chapter_url=?3"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(version_id)
            .bind(id)
            .bind(chapter_url)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(map_version))
    }

    /// 读正文池里的原始字节（可能仍是压缩状态）。
    pub async fn get_content_blob(
        &self,
        content_id: i64,
    ) -> Result<Option<(i64, Vec<u8>)>, AppError> {
        let row = sqlx::query("SELECT codec, content FROM chapter_contents WHERE content_id=?1")
            .bind(content_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get::<i64, _>("codec"), r.get::<Vec<u8>, _>("content"))))
    }

    /// 追加一个版本。内容与当前同类版本相同时只更新检查时间，不新增时间线。
    pub async fn record_version(
        &self,
        input: NewChapterVersion<'_>,
    ) -> Result<RecordOutcome, AppError> {
        let Some(book_id) = parse_book_id(input.book_id) else {
            return Err(AppError::BadRequest(format!(
                "bookId 格式非法: {:?}",
                input.book_id
            )));
        };
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let outcome = record_version_inner(&mut tx, book_id, &input).await?;
        tx.commit().await?;
        Ok(outcome)
    }

    /// 批量追加版本：每 [`RECORD_VERSION_BATCH_SIZE`] 章一个 `BEGIN IMMEDIATE`
    /// 事务，块内逐章 `SAVEPOINT`；单章失败 `ROLLBACK TO SAVEPOINT` 撤销本章的
    /// 部分写入、记入失败并继续后续章节，块末统一 commit。
    ///
    /// 返回与 `inputs` 等长、按顺序对齐的逐章结果，调用方据此计数。
    /// 整块失败（事务开不了 / 提交失败 / 保存点机制损坏）时，受影响章节
    /// 全部记为失败，不影响其他块。
    pub async fn record_versions_batch(
        &self,
        inputs: &[NewChapterVersion<'_>],
    ) -> Vec<Result<RecordOutcome, AppError>> {
        let mut results: Vec<Result<RecordOutcome, AppError>> = Vec::with_capacity(inputs.len());
        for chunk in inputs.chunks(RECORD_VERSION_BATCH_SIZE) {
            let chunk_start = results.len();
            let mut tx = match self.pool.begin_with("BEGIN IMMEDIATE").await {
                Ok(tx) => tx,
                Err(error) => {
                    let message = format!("批量写入事务开启失败: {error}");
                    results.extend(
                        chunk
                            .iter()
                            .map(|_| Err(AppError::Internal(anyhow::anyhow!(message.clone())))),
                    );
                    continue;
                }
            };
            // 保存点/回滚本身失败说明连接或事务已损坏：本章记失败，块内剩余
            // 章节标记为未写入，事务随作用域丢弃即整体回滚，下一块换新事务。
            let mut fatal = false;
            for input in chunk {
                if fatal {
                    results.push(Err(AppError::Internal(anyhow::anyhow!(
                        "所在批量写入块已中止，本章未写入"
                    ))));
                    continue;
                }
                if let Err(error) = sqlx::query("SAVEPOINT chapter_version")
                    .execute(&mut *tx)
                    .await
                {
                    results.push(Err(AppError::Db(error)));
                    fatal = true;
                    continue;
                }
                let outcome = match parse_book_id(input.book_id) {
                    Some(book_id) => record_version_inner(&mut tx, book_id, input).await,
                    None => Err(AppError::BadRequest(format!(
                        "bookId 格式非法: {:?}",
                        input.book_id
                    ))),
                };
                match outcome {
                    Ok(outcome) => match sqlx::query("RELEASE SAVEPOINT chapter_version")
                        .execute(&mut *tx)
                        .await
                    {
                        Ok(_) => results.push(Ok(outcome)),
                        Err(error) => {
                            results.push(Err(AppError::Db(error)));
                            fatal = true;
                        }
                    },
                    Err(error) => {
                        let recovered = sqlx::query("ROLLBACK TO SAVEPOINT chapter_version")
                            .execute(&mut *tx)
                            .await
                            .is_ok()
                            // ROLLBACK TO 之后保存点仍在，释放掉下一章才能复用同名保存点
                            && sqlx::query("RELEASE SAVEPOINT chapter_version")
                                .execute(&mut *tx)
                                .await
                                .is_ok();
                        results.push(Err(error));
                        if !recovered {
                            fatal = true;
                        }
                    }
                }
            }
            if fatal {
                // 事务随 Drop 整体回滚：本块已报成功的章节实际未落库，
                // 与提交失败一样改记失败
                mark_chunk_aborted(&mut results[chunk_start..], "批量写入事务中止");
                continue;
            }
            if let Err(error) = tx.commit().await {
                // 提交失败整块回滚：本块已报成功的章节改记失败
                let message = format!("批量写入提交失败: {error}");
                mark_chunk_aborted(&mut results[chunk_start..], &message);
            }
        }
        results
    }
}

/// 块级事务失败（提交失败 / 保存点机制损坏导致事务随 Drop 回滚）时，
/// 把本块已报成功的章节统一改记失败：这些章节的写入实际未落库。
/// 已记失败的条目（单章校验失败、保存点报错、中止后未写入）保持原样。
fn mark_chunk_aborted(results: &mut [Result<RecordOutcome, AppError>], message: &str) {
    for result in results.iter_mut() {
        if result.is_ok() {
            *result = Err(AppError::Internal(anyhow::anyhow!(message.to_string())));
        }
    }
}

/// 批量写入时每个事务块容纳的章节数：几百次插入对 SQLite 一个事务毫无压力，
/// 块再大只是拉长单事务持锁时间，收益递减。
pub const RECORD_VERSION_BATCH_SIZE: usize = 300;

/// 单章写入的全部 SQL，在调用方的事务里执行。
///
/// 单章入口（[`ChapterContentRepo::record_version`]）包一层
/// `BEGIN IMMEDIATE` + commit；批量入口（`record_versions_batch`）
/// 逐章套 SAVEPOINT 复用本函数。
async fn record_version_inner(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    book_id: i64,
    input: &NewChapterVersion<'_>,
) -> Result<RecordOutcome, AppError> {
    let latest = sqlx::query(
        "SELECT latest_upstream_version_id, latest_user_version_id \
             FROM chapter_latest WHERE book_id=?1 AND chapter_url=?2",
    )
    .bind(book_id)
    .bind(input.chapter_url)
    .fetch_optional(&mut **tx)
    .await?;
    let (latest_upstream, latest_user) = match &latest {
        Some(row) => (
            row.get::<Option<i64>, _>("latest_upstream_version_id"),
            row.get::<Option<i64>, _>("latest_user_version_id"),
        ),
        None => (None, None),
    };

    let compare_id = if input.kind == "upstream" {
        latest_upstream
    } else {
        latest_user
    };
    if let Some(existing) = compare_id {
        let same: Option<i64> = sqlx::query_scalar(
            "SELECT v.version_id FROM chapter_versions v \
                 JOIN chapter_contents c ON c.content_id=v.content_id \
                 WHERE v.version_id=?1 AND c.content_hash=?2",
        )
        .bind(existing)
        .bind(input.content_hash)
        .fetch_optional(&mut **tx)
        .await?;
        if same.is_some() {
            let now = now_millis();
            if input.kind == "upstream" {
                sqlx::query(
                    "UPDATE chapter_latest SET last_attempt_at=?3, last_success_at=?3, last_error='' \
                         WHERE book_id=?1 AND chapter_url=?2",
                )
                .bind(book_id)
                .bind(input.chapter_url)
                .bind(now)
                .execute(&mut **tx)
                .await?;
            } else if input.make_current {
                sqlx::query(
                    "UPDATE chapter_latest SET edited_at=?3 WHERE book_id=?1 AND chapter_url=?2",
                )
                .bind(book_id)
                .bind(input.chapter_url)
                .bind(now)
                .execute(&mut **tx)
                .await?;
                sqlx::query(
                    "DELETE FROM chapter_view_state WHERE user_ns=?1 AND book_id=?2 AND chapter_url=?3",
                )
                .bind(input.user_ns)
                .bind(book_id)
                .bind(input.chapter_url)
                .execute(&mut **tx)
                .await?;
            }
            return Ok(RecordOutcome::Unchanged {
                version_id: existing,
            });
        }
    }

    let content_id: i64 =
        match sqlx::query_scalar("SELECT content_id FROM chapter_contents WHERE content_hash=?1")
            .bind(input.content_hash)
            .fetch_optional(&mut **tx)
            .await?
        {
            Some(id) => id,
            None => {
                let result = sqlx::query(
                    "INSERT INTO chapter_contents \
                     (content_hash, codec, raw_size, stored_size, content, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .bind(input.content_hash)
                .bind(input.codec)
                .bind(input.raw_size)
                .bind(input.stored_size)
                .bind(input.content)
                .bind(now_millis())
                .execute(&mut **tx)
                .await?;
                result.last_insert_rowid()
            }
        };

    let now = now_millis();
    let version_created_at = input.created_at.unwrap_or(now);
    let result = sqlx::query(
        "INSERT INTO chapter_versions \
             (book_id, chapter_url, content_id, kind, book_source_id, book_source_name, \
              base_version_id, chapter_index, chapter_title, book_url, fetch_url, note, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )
    .bind(book_id)
    .bind(input.chapter_url)
    .bind(content_id)
    .bind(input.kind)
    .bind(input.book_source_id)
    .bind(input.book_source_name)
    .bind(input.base_version_id)
    .bind(input.chapter_index)
    .bind(input.chapter_title)
    .bind(input.book_url)
    .bind(input.fetch_url)
    .bind(input.note)
    .bind(version_created_at)
    .execute(&mut **tx)
    .await?;
    let version_id = result.last_insert_rowid();

    let became_current = input.kind == "upstream" || input.make_current;
    sqlx::query(
        "INSERT INTO chapter_latest \
             (book_id, chapter_url, latest_upstream_version_id, latest_user_version_id, \
              last_attempt_at, last_success_at, upstream_changed_at, edited_at, last_error) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?5, ?6, '') \
             ON CONFLICT(book_id, chapter_url) DO UPDATE SET \
               latest_upstream_version_id=COALESCE(excluded.latest_upstream_version_id, chapter_latest.latest_upstream_version_id), \
               latest_user_version_id=COALESCE(excluded.latest_user_version_id, chapter_latest.latest_user_version_id), \
               last_attempt_at=COALESCE(excluded.last_attempt_at, chapter_latest.last_attempt_at), \
               last_success_at=COALESCE(excluded.last_success_at, chapter_latest.last_success_at), \
               upstream_changed_at=COALESCE(excluded.upstream_changed_at, chapter_latest.upstream_changed_at), \
               edited_at=COALESCE(excluded.edited_at, chapter_latest.edited_at), \
               last_error=CASE WHEN excluded.latest_upstream_version_id IS NOT NULL \
                               THEN '' ELSE chapter_latest.last_error END",
    )
    .bind(book_id)
    .bind(input.chapter_url)
    .bind(if input.kind == "upstream" {
        Some(version_id)
    } else {
        None
    })
    .bind(if input.kind != "upstream" && input.make_current {
        Some(version_id)
    } else {
        None
    })
    .bind(if input.kind == "upstream" {
        Some(now)
    } else {
        None
    })
    .bind(if input.kind != "upstream" && input.make_current {
        Some(now)
    } else {
        None
    })
    .execute(&mut **tx)
    .await?;

    // 保存新的用户版本时清掉手动选择，让新版本成为当前。
    if input.kind != "upstream" && input.make_current {
        sqlx::query(
            "DELETE FROM chapter_view_state WHERE user_ns=?1 AND book_id=?2 AND chapter_url=?3",
        )
        .bind(input.user_ns)
        .bind(book_id)
        .bind(input.chapter_url)
        .execute(&mut **tx)
        .await?;
    }

    Ok(RecordOutcome::Appended {
        version_id,
        became_current,
    })
}

impl ChapterContentRepo {
    /// 回源失败：只更新尝试时间与错误，不动缓存与指针。
    pub async fn touch_upstream_failure(
        &self,
        book_id: &str,
        chapter_url: &str,
        error: &str,
    ) -> Result<(), AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(());
        };
        sqlx::query(
            "UPDATE chapter_latest SET last_attempt_at=?3, last_error=?4 \
             WHERE book_id=?1 AND chapter_url=?2",
        )
        .bind(id)
        .bind(chapter_url)
        .bind(now_millis())
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 某用户在某章手动选中的版本；没有选择返回 None。
    pub async fn selected_version_id(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<Option<i64>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(None);
        };
        let selected: Option<i64> = sqlx::query_scalar(
            "SELECT selected_version_id FROM chapter_view_state \
             WHERE user_ns=?1 AND book_id=?2 AND chapter_url=?3",
        )
        .bind(user_ns)
        .bind(id)
        .bind(chapter_url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(selected)
    }

    /// 设置某用户在某章选中的版本；删除该行表示跟随最新。
    pub async fn set_view_state(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
        selected_version_id: Option<i64>,
    ) -> Result<(), AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Err(AppError::BadRequest(format!(
                "bookId 格式非法: {book_id:?}"
            )));
        };
        if let Some(version_id) = selected_version_id {
            let owns: Option<i64> = sqlx::query_scalar(
                "SELECT version_id FROM chapter_versions \
                 WHERE version_id=?1 AND book_id=?2 AND chapter_url=?3",
            )
            .bind(version_id)
            .bind(id)
            .bind(chapter_url)
            .fetch_optional(&self.pool)
            .await?;
            if owns.is_none() {
                return Err(AppError::NotFound("章节版本不存在".to_string()));
            }
        }
        match selected_version_id {
            Some(version_id) => {
                sqlx::query(
                    "INSERT INTO chapter_view_state \
                     (user_ns, book_id, chapter_url, selected_version_id, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(user_ns, book_id, chapter_url) DO UPDATE SET \
                       selected_version_id=excluded.selected_version_id, updated_at=excluded.updated_at",
                )
                .bind(user_ns)
                .bind(id)
                .bind(chapter_url)
                .bind(version_id)
                .bind(now_millis())
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query(
                    "DELETE FROM chapter_view_state WHERE user_ns=?1 AND book_id=?2 AND chapter_url=?3",
                )
                .bind(user_ns)
                .bind(id)
                .bind(chapter_url)
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }

    /// 清除当前用户版本（删除编辑版 / 恢复上游）。历史不删。
    /// 如果视图状态正好指向被清掉的版本，一并删除；指向其他版本则保留。
    pub async fn clear_current_user_version(
        &self,
        user_ns: &str,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<(), AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(());
        };
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "UPDATE chapter_latest SET latest_user_version_id=NULL, edited_at=NULL \
             WHERE book_id=?1 AND chapter_url=?2",
        )
        .bind(id)
        .bind(chapter_url)
        .execute(&mut *tx)
        .await?;
        // 恢复上游时，如果用户正钉在某个用户版本上，把这条选择一并清掉；
        // 钉的是上游版本则保留（他本来就在看上游）。
        sqlx::query(
            "DELETE FROM chapter_view_state \
             WHERE user_ns=?1 AND book_id=?2 AND chapter_url=?3 \
               AND selected_version_id IN (SELECT version_id FROM chapter_versions \
                                           WHERE book_id=?2 AND chapter_url=?3 AND kind != 'upstream')",
        )
        .bind(user_ns)
        .bind(id)
        .bind(chapter_url)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 整书换回最新上游：摘掉本书所有章节的用户版本指针，并清掉该用户的
    /// 全部版本选择，每章回落到最新上游（本地书回到原文）。历史版本严格保留。
    /// 返回摘掉用户版本指针的章节数。
    pub async fn reset_book_to_upstream(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<u64, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(0);
        };
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let cleared = sqlx::query(
            "UPDATE chapter_latest SET latest_user_version_id=NULL, edited_at=NULL \
             WHERE book_id=?1 AND latest_user_version_id IS NOT NULL",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        sqlx::query("DELETE FROM chapter_view_state WHERE user_ns=?1 AND book_id=?2")
            .bind(user_ns)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(cleared)
    }

    /// 时间线：按 version_id 倒序，游标为上一页最小的 version_id。
    pub async fn list_timeline(
        &self,
        book_id: &str,
        chapter_url: &str,
        limit: i64,
        before_version_id: Option<i64>,
    ) -> Result<Vec<ChapterVersionRow>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(Vec::new());
        };
        // 手工导入的版本可以带更早的 created_at，时间线按版本时间排，而不是
        // 按插入顺序排；游标也要同时带上 created_at。
        let cursor_created_at = match before_version_id {
            Some(before) => {
                let row = sqlx::query(
                    "SELECT created_at FROM chapter_versions \
                     WHERE version_id=?1 AND book_id=?2 AND chapter_url=?3",
                )
                .bind(before)
                .bind(id)
                .bind(chapter_url)
                .fetch_optional(&self.pool)
                .await?;
                match row {
                    Some(row) => Some(row.get::<i64, _>("created_at")),
                    None => return Ok(Vec::new()),
                }
            }
            None => None,
        };
        let sql = format!(
            "SELECT {VERSION_COLS}
             FROM chapter_versions v
             JOIN chapter_contents c ON c.content_id = v.content_id
             WHERE v.book_id=?1 AND v.chapter_url=?2
               AND (?3 IS NULL OR v.created_at < ?3
                    OR (v.created_at = ?3 AND v.version_id < ?4))
             ORDER BY v.created_at DESC, v.version_id DESC LIMIT ?5"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(chapter_url)
            .bind(cursor_created_at)
            .bind(before_version_id)
            .bind(limit.clamp(1, 200))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(map_version).collect())
    }

    /// 某本书里当前显示为用户版本的章节 URL（目录徽章用）。
    pub async fn list_user_version_urls(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Vec<String>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT l.chapter_url
             FROM chapter_latest l
             LEFT JOIN chapter_view_state s
               ON s.user_ns=?1 AND s.book_id=l.book_id AND s.chapter_url=l.chapter_url
             JOIN chapter_versions v
               ON v.version_id = COALESCE(s.selected_version_id, l.latest_user_version_id, l.latest_upstream_version_id)
               AND v.book_id=l.book_id AND v.chapter_url=l.chapter_url
             WHERE l.book_id=?2 AND v.kind != 'upstream'
             ORDER BY l.chapter_url",
        )
        .bind(user_ns)
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| r.get::<String, _>("chapter_url"))
            .collect())
    }

    /// 每章当前显示版本的标记（阅读器目录标签用）：
    /// 有效版本 = 手动选择 > 最近用户版本 > 最新上游。只返回有版本记录的章；
    /// 没有任何版本的章（从未读过）前端按「最新上游」处理。
    pub async fn list_version_marks(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Vec<ChapterVersionMarkRow>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT l.chapter_url, v.kind, v.note, v.created_at, \
                    COALESCE(v.version_id = l.latest_upstream_version_id, 0) AS is_latest_upstream
             FROM chapter_latest l
             LEFT JOIN chapter_view_state s
               ON s.user_ns=?1 AND s.book_id=l.book_id AND s.chapter_url=l.chapter_url
             JOIN chapter_versions v
               ON v.version_id = COALESCE(s.selected_version_id, l.latest_user_version_id, l.latest_upstream_version_id)
               AND v.book_id=l.book_id AND v.chapter_url=l.chapter_url
             WHERE l.book_id=?2",
        )
        .bind(user_ns)
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| ChapterVersionMarkRow {
                chapter_url: r.get::<String, _>("chapter_url"),
                kind: r.get::<String, _>("kind"),
                note: r.get::<String, _>("note"),
                created_at: r.get::<i64, _>("created_at"),
                is_latest_upstream: r.get::<i64, _>("is_latest_upstream") != 0,
            })
            .collect())
    }

    /// 一本书的全部当前版本元数据（全文检索按章读取用）。
    pub async fn list_effective_versions(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Vec<EffectiveVersionRow>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT l.chapter_url, v.content_id, v.kind,
                    v.chapter_index, v.chapter_title, c.raw_size
             FROM chapter_latest l
             LEFT JOIN chapter_view_state s
               ON s.user_ns=?1 AND s.book_id=l.book_id AND s.chapter_url=l.chapter_url
             JOIN chapter_versions v
               ON v.version_id = COALESCE(s.selected_version_id, l.latest_user_version_id, l.latest_upstream_version_id)
               AND v.book_id=l.book_id AND v.chapter_url=l.chapter_url
             JOIN chapter_contents c ON c.content_id = v.content_id
             WHERE l.book_id=?2
             ORDER BY v.chapter_index, v.version_id",
        )
        .bind(user_ns)
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| EffectiveVersionRow {
                chapter_url: r.get("chapter_url"),
                content_id: r.get("content_id"),
                kind: r.get("kind"),
                chapter_index: r.get::<i64, _>("chapter_index") as i32,
                chapter_title: r.get("chapter_title"),
                raw_size: r.get("raw_size"),
            })
            .collect())
    }

    /// 一本书全部章节当前显示版本的正文（编码形态，供预览归一化比较）。
    pub async fn list_effective_content_blobs(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Vec<EffectiveContentBlob>, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT l.chapter_url, c.codec, c.content
             FROM chapter_latest l
             LEFT JOIN chapter_view_state s
               ON s.user_ns=?1 AND s.book_id=l.book_id AND s.chapter_url=l.chapter_url
             JOIN chapter_versions v
               ON v.version_id = COALESCE(s.selected_version_id, l.latest_user_version_id, l.latest_upstream_version_id)
               AND v.book_id=l.book_id AND v.chapter_url=l.chapter_url
             JOIN chapter_contents c ON c.content_id = v.content_id
             WHERE l.book_id=?2",
        )
        .bind(user_ns)
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| EffectiveContentBlob {
                chapter_url: r.get("chapter_url"),
                codec: r.get("codec"),
                content: r.get("content"),
            })
            .collect())
    }

    /// 图片源的空正文只记检查时间，不往时间线里塞 0 字节版本。
    pub async fn touch_empty_upstream(
        &self,
        book_id: &str,
        chapter_url: &str,
    ) -> Result<(), AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(());
        };
        let now = now_millis();
        sqlx::query(
            "INSERT INTO chapter_latest \
             (book_id, chapter_url, latest_upstream_version_id, latest_user_version_id, \
              last_attempt_at, last_success_at, upstream_changed_at, edited_at, last_error) \
             VALUES (?1, ?2, NULL, NULL, ?3, ?3, NULL, NULL, '') \
             ON CONFLICT(book_id, chapter_url) DO UPDATE SET \
               last_attempt_at=excluded.last_attempt_at, \
               last_success_at=excluded.last_success_at, \
               last_error=''",
        )
        .bind(id)
        .bind(chapter_url)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn has_content(&self, book_id: &str, chapter_url: &str) -> Result<bool, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(false);
        };
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM chapter_latest \
             WHERE book_id=?1 AND chapter_url=?2 \
               AND (latest_upstream_version_id IS NOT NULL OR latest_user_version_id IS NOT NULL)",
        )
        .bind(id)
        .bind(chapter_url)
        .fetch_one(&self.pool)
        .await?;
        Ok(count > 0)
    }

    pub async fn count_chapters(&self, book_id: &str) -> Result<u64, AppError> {
        let Some(id) = parse_book_id(book_id) else {
            return Ok(0);
        };
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM chapter_latest \
             WHERE book_id=?1 AND (latest_upstream_version_id IS NOT NULL OR latest_user_version_id IS NOT NULL)",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(count as u64)
    }

    /// 正文历史占用：正文池条数、版本数、章节数、落库字节数。
    pub async fn stats(&self) -> Result<(u64, u64, u64, u64), AppError> {
        let contents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chapter_contents")
            .fetch_one(&self.pool)
            .await?;
        let bytes: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(stored_size), 0) FROM chapter_contents")
                .fetch_one(&self.pool)
                .await?;
        let versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chapter_versions")
            .fetch_one(&self.pool)
            .await?;
        let chapters: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM chapter_latest \
             WHERE latest_upstream_version_id IS NOT NULL OR latest_user_version_id IS NOT NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((
            contents as u64,
            versions as u64,
            chapters as u64,
            bytes as u64,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 块中止/提交失败的修正循环：已报成功（Appended/Unchanged）的条目
    /// 改记为 Internal 失败，此前已失败的条目保持原文案不变。
    #[test]
    fn mark_chunk_aborted_rewrites_ok_and_keeps_err() {
        let mut results: Vec<Result<RecordOutcome, AppError>> = vec![
            Ok(RecordOutcome::Appended {
                version_id: 1,
                became_current: true,
            }),
            Err(AppError::BadRequest("bookId 格式非法".to_string())),
            Ok(RecordOutcome::Unchanged { version_id: 2 }),
            Err(AppError::Internal(anyhow::anyhow!(
                "所在批量写入块已中止，本章未写入"
            ))),
        ];
        mark_chunk_aborted(&mut results, "批量写入事务中止");
        assert!(
            matches!(&results[0], Err(AppError::Internal(error)) if error.to_string() == "批量写入事务中止"),
            "Appended 应改记为中止失败"
        );
        assert!(
            matches!(&results[1], Err(AppError::BadRequest(_))),
            "已有的 BadRequest 应保持原样"
        );
        assert!(
            matches!(&results[2], Err(AppError::Internal(error)) if error.to_string() == "批量写入事务中止"),
            "Unchanged 应改记为中止失败"
        );
        assert!(
            matches!(&results[3], Err(AppError::Internal(error)) if error.to_string() == "所在批量写入块已中止，本章未写入"),
            "已有的失败文案应保持原样"
        );
    }
}
