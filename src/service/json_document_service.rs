use serde::Serialize;
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use crate::error::error::AppError;
use crate::util::time::now_ts;

/// 通用 JSON 文档存取（userdata、AI 模型配置等无结构键值）。
///
/// 书签与替换规则已拆成实体表（0002 迁移），不再走这张表；
/// legacy 文件读穿（老部署 `data/<ns>/<name>`）已随破坏性升级口径移除。
#[derive(Clone)]
pub struct JsonDocumentService {
    pool: SqlitePool,
}

impl JsonDocumentService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get_value(&self, user_ns: &str, name: &str) -> Result<Option<Value>, AppError> {
        let Some(json) = self.get_raw(user_ns, name).await? else {
            return Ok(None);
        };
        serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| AppError::BadRequest(e.to_string()))
    }

    pub async fn set_value<T: Serialize + ?Sized>(
        &self,
        user_ns: &str,
        name: &str,
        value: &T,
    ) -> Result<(), AppError> {
        let json = serde_json::to_string(value).map_err(|e| AppError::BadRequest(e.to_string()))?;
        self.set_raw(user_ns, name, &json).await
    }

    async fn get_raw(&self, user_ns: &str, name: &str) -> Result<Option<String>, AppError> {
        let row = sqlx::query("SELECT json FROM json_documents WHERE user_ns=?1 AND name=?2")
            .bind(user_ns)
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| row.get::<String, _>("json")))
    }

    async fn set_raw(&self, user_ns: &str, name: &str, json: &str) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO json_documents (user_ns, name, json, updated_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(user_ns, name) DO UPDATE SET json=excluded.json, updated_at=excluded.updated_at",
        )
        .bind(user_ns)
        .bind(name)
        .bind(json)
        .bind(now_ts() * 1000)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
