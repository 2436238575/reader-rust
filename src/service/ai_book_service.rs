use sqlx::{Row, SqlitePool};

use crate::error::error::AppError;
use crate::model::ai_book::AiBookMemory;
use crate::util::time::now_ts;

/// AI 书籍记忆：按 bookId 键控（`book_id` 列存 `book_<n>`，关联 books 登记表）。
/// `book_url` 列是冗余数据，供 WebDAV 恢复后校验「同 id 同书」防串书。
#[derive(Clone)]
pub struct AiBookService {
    pool: SqlitePool,
}

impl AiBookService {
    pub fn new(pool: SqlitePool, _storage_dir: &str) -> Self {
        Self { pool }
    }

    pub async fn get(
        &self,
        user_ns: &str,
        book_id: &str,
    ) -> Result<Option<AiBookMemory>, AppError> {
        if let Some(row) =
            sqlx::query("SELECT json FROM ai_book_memories WHERE user_ns=?1 AND book_id=?2")
                .bind(user_ns)
                .bind(book_id)
                .fetch_optional(&self.pool)
                .await?
        {
            let json: String = row.get("json");
            let memory = serde_json::from_str::<AiBookMemory>(&json)
                .map_err(|e| AppError::BadRequest(e.to_string()))?;
            return Ok(Some(memory));
        }
        Ok(None)
    }

    pub async fn save_for_book(
        &self,
        user_ns: &str,
        book_id: &str,
        book_url: &str,
        mut memory: AiBookMemory,
    ) -> Result<AiBookMemory, AppError> {
        if book_id.trim().is_empty() {
            return Err(AppError::BadRequest("bookId required".to_string()));
        }
        if memory.book_url.trim().is_empty() {
            memory.book_url = book_url.to_string();
        }
        if memory.book_url != book_url {
            return Err(AppError::BadRequest("bookUrl mismatch".to_string()));
        }
        if memory.updated_at <= 0 {
            memory.updated_at = now_ts() * 1000;
        }
        memory.book_id = Some(book_id.to_string());

        let data =
            serde_json::to_string(&memory).map_err(|e| AppError::BadRequest(e.to_string()))?;
        sqlx::query(
            "INSERT INTO ai_book_memories (user_ns, book_id, book_url, json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(user_ns, book_id) DO UPDATE SET book_url=excluded.book_url, json=excluded.json, updated_at=excluded.updated_at",
        )
        .bind(user_ns)
        .bind(book_id)
        .bind(book_url)
        .bind(data)
        .bind(memory.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(memory)
    }

    pub async fn delete(&self, user_ns: &str, book_id: &str) -> Result<bool, AppError> {
        let result = sqlx::query("DELETE FROM ai_book_memories WHERE user_ns=?1 AND book_id=?2")
            .bind(user_ns)
            .bind(book_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 列出全部记忆的 `(book_id, book_url)`：WebDAV 恢复重建登记表后，
    /// 由调用方逐行比对，book_url 与新登记同 id 行不一致的记忆删除（防串书）。
    pub async fn list_keys(&self, user_ns: &str) -> Result<Vec<(String, String)>, AppError> {
        let rows = sqlx::query("SELECT book_id, book_url FROM ai_book_memories WHERE user_ns=?1")
            .bind(user_ns)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<String, _>("book_id"),
                    r.get::<String, _>("book_url"),
                )
            })
            .collect())
    }
}
