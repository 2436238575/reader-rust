use std::path::PathBuf;

use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use sqlx::{Row, SqlitePool};
use tokio::fs;

use crate::error::error::AppError;
use crate::util::time::now_ts;

#[derive(Clone)]
pub struct JsonDocumentService {
    pool: SqlitePool,
    storage_dir: PathBuf,
}

impl JsonDocumentService {
    pub fn new(pool: SqlitePool, storage_dir: &str) -> Self {
        Self {
            pool,
            storage_dir: PathBuf::from(storage_dir),
        }
    }

    pub async fn get_value(&self, user_ns: &str, name: &str) -> Result<Option<Value>, AppError> {
        if let Some(json) = self.get_raw(user_ns, name).await? {
            let value =
                serde_json::from_str(&json).map_err(|e| AppError::BadRequest(e.to_string()))?;
            return Ok(Some(value));
        }

        let path = self.legacy_user_path(user_ns, name);
        if !path.exists() {
            return Ok(None);
        }
        let json = fs::read_to_string(&path)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let value = serde_json::from_str::<Value>(&json)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        self.set_raw(user_ns, name, &json).await?;
        Ok(Some(value))
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

    pub async fn read_list<T: DeserializeOwned>(
        &self,
        user_ns: &str,
        name: &str,
    ) -> Result<Vec<T>, AppError> {
        let Some(value) = self.get_value(user_ns, name).await? else {
            return Ok(Vec::new());
        };
        serde_json::from_value(value).map_err(|e| AppError::BadRequest(e.to_string()))
    }

    pub async fn write_list<T: Serialize>(
        &self,
        user_ns: &str,
        name: &str,
        list: &Vec<T>,
    ) -> Result<(), AppError> {
        self.set_value(user_ns, name, list).await
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

    fn legacy_user_path(&self, user_ns: &str, name: &str) -> PathBuf {
        self.storage_dir.join("data").join(user_ns).join(name)
    }
}
