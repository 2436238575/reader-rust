use std::collections::HashMap;

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

    /// 批量读取：按传入顺序返回 `(name, Some(value) | None)`，缺失的文档为
    /// `None`。单条 IN 查询拿回全部命中行后在内存里对位，调用方（批量
    /// userdata 接口）上限 20 个名字，远低于 SQLite 的绑定参数上限。
    pub async fn get_many(
        &self,
        user_ns: &str,
        names: &[String],
    ) -> Result<Vec<(String, Option<Value>)>, AppError> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (2..=names.len() + 1)
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT name, json FROM json_documents WHERE user_ns=?1 AND name IN ({placeholders})"
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(user_ns);
        for name in names {
            query = query.bind(name);
        }
        let rows = query.fetch_all(&self.pool).await?;
        let mut found: HashMap<String, Value> = HashMap::with_capacity(rows.len());
        for row in rows {
            let name: String = row.get("name");
            let json: String = row.get("json");
            let value =
                serde_json::from_str(&json).map_err(|e| AppError::BadRequest(e.to_string()))?;
            found.insert(name, value);
        }
        Ok(names
            .iter()
            .map(|name| (name.clone(), found.remove(name)))
            .collect())
    }

    /// 批量写入：单个事务里逐条做与 `set_raw` 相同的 UPSERT，任一条失败
    /// 整批回滚（已写入的部分不落库）。文档名合法性/体积上限是调用方
    /// （handler）的职责，这里只保证「要么全写、要么全不写」。
    pub async fn set_many(
        &self,
        user_ns: &str,
        docs: &[(String, Value)],
    ) -> Result<usize, AppError> {
        let mut tx = self.pool.begin().await?;
        for (name, value) in docs {
            let json =
                serde_json::to_string(value).map_err(|e| AppError::BadRequest(e.to_string()))?;
            sqlx::query(
                "INSERT INTO json_documents (user_ns, name, json, updated_at) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(user_ns, name) DO UPDATE SET json=excluded.json, updated_at=excluded.updated_at",
            )
            .bind(user_ns)
            .bind(name)
            .bind(&json)
            .bind(now_ts() * 1000)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(docs.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db;
    use crate::util::crypto::random_string;
    use std::path::PathBuf;

    async fn service() -> (JsonDocumentService, SqlitePool, PathBuf) {
        crate::util::test_temp::clean_stale_test_dirs();
        let dir = std::env::temp_dir().join(format!("reader-rust-jsondoc-{}", random_string(8)));
        std::fs::create_dir_all(&dir).unwrap();
        let database_url = format!("sqlite:{}?mode=rwc", dir.join("reader.db").display());
        let pool = db::init_pool(&database_url).await.unwrap();
        (JsonDocumentService::new(pool.clone()), pool, dir)
    }

    async fn finish(pool: SqlitePool, dir: PathBuf) {
        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn get_many_keeps_input_order_and_reports_missing_as_none() {
        let (svc, pool, dir) = service().await;
        svc.set_value("user_1", "a", &serde_json::json!({ "x": 1 }))
            .await
            .unwrap();
        svc.set_value("user_1", "b", &serde_json::json!([1, 2]))
            .await
            .unwrap();

        let names = vec!["b".to_string(), "missing".to_string(), "a".to_string()];
        let got = svc.get_many("user_1", &names).await.unwrap();
        assert_eq!(got.len(), 3, "结果应与传入名字一一对应");
        assert_eq!(got[0].0, "b");
        assert_eq!(got[0].1, Some(serde_json::json!([1, 2])));
        assert_eq!(got[1].0, "missing");
        assert_eq!(got[1].1, None, "缺失文档应为 None");
        assert_eq!(got[2].1, Some(serde_json::json!({ "x": 1 })));

        // 命名空间隔离：别的 ns 读不到
        let got = svc.get_many("user_2", &names).await.unwrap();
        assert!(got.iter().all(|(_, v)| v.is_none()));
        // 空列表直接空返回，不产生 SQL
        assert!(svc.get_many("user_1", &[]).await.unwrap().is_empty());
        finish(pool, dir).await;
    }

    #[tokio::test]
    async fn set_many_upserts_and_returns_written_count() {
        let (svc, pool, dir) = service().await;
        svc.set_value("user_1", "a", &serde_json::json!(1))
            .await
            .unwrap();
        let written = svc
            .set_many(
                "user_1",
                &[
                    ("a".to_string(), serde_json::json!(2)), // 覆盖已有文档
                    ("b".to_string(), serde_json::json!({ "y": true })),
                ],
            )
            .await
            .unwrap();
        assert_eq!(written, 2);
        assert_eq!(
            svc.get_value("user_1", "a").await.unwrap(),
            Some(serde_json::json!(2))
        );
        assert_eq!(
            svc.get_value("user_1", "b").await.unwrap(),
            Some(serde_json::json!({ "y": true }))
        );
        finish(pool, dir).await;
    }

    /// 任一条失败整批回滚：用 BEFORE INSERT 触发器让第二个文档在**事务
    /// 中途**报错（模拟磁盘/约束等真实 DB 故障，handler 层的预校验挡不住
    /// 的那类），断言第一条已执行的写入也没有落库。
    #[tokio::test]
    async fn set_many_rolls_back_whole_batch_when_any_write_fails() {
        let (svc, pool, dir) = service().await;
        sqlx::query(
            "CREATE TRIGGER fail_on_boom BEFORE INSERT ON json_documents \
             WHEN NEW.name='boom-doc' BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .execute(&pool)
        .await
        .unwrap();

        let err = svc
            .set_many(
                "user_1",
                &[
                    ("first-doc".to_string(), serde_json::json!({ "ok": 1 })),
                    ("boom-doc".to_string(), serde_json::json!({ "bad": 1 })),
                ],
            )
            .await;
        assert!(err.is_err(), "第二条写入失败应把错误冒出来");

        let row = sqlx::query("SELECT json FROM json_documents WHERE user_ns='user_1'")
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(row.is_none(), "事务中途失败时第一批的写入也必须回滚");
        finish(pool, dir).await;
    }
}
