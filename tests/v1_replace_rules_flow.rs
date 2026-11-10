//! `/v1/me/replace-rules` 的集成测试：CRUD 往返、按名 upsert 的 201/200、
//! merge-patch、幂等删除、校验 422、批量写删。

use axum::http::StatusCode;
use serde_json::{json, Value};
use std::path::PathBuf;

use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;

mod common;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-replace-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = common::base_config(&temp_dir, "v1-replace-test-secret");
        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let (base_url, _server) = common::spawn_app(app).await;
        let client = reqwest::Client::new();
        let token = common::login(&client, &base_url).await;
        Self {
            base_url,
            temp_dir,
            client,
            token,
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

#[tokio::test]
async fn replace_rules_require_auth_with_problem_401() {
    let server = TestServer::start().await;
    let resp = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");
    server.cleanup();
}

#[tokio::test]
async fn replace_rules_crud_roundtrip() {
    let server = TestServer::start().await;

    // 新建：201 + 条目
    let created = server
        .client
        .post(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .json(&json!({
            "name": "去广告",
            "pattern": "广告.*",
            "replacement": "",
            "isRegex": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let body: Value = created.json().await.unwrap();
    assert_eq!(body["name"], json!("去广告"));
    assert_eq!(body["isRegex"], json!(true));
    assert_eq!(body["isEnabled"], json!(true), "缺省启用");

    // 同名再建：200 整体替换
    let replaced = server
        .client
        .post(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .json(&json!({
            "name": "去广告",
            "pattern": "广告词.*",
            "replacement": "[已移除]",
            "isRegex": false,
            "scope": "book:1",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(replaced.status(), StatusCode::OK);

    // 列表：一条，字段完整
    let list: Value = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["pattern"], json!("广告词.*"));
    assert_eq!(list[0]["replacement"], json!("[已移除]"));
    assert_eq!(list[0]["scope"], json!("book:1"));

    // merge-patch：只改 pattern
    let patched = server
        .client
        .patch(server.url("/v1/me/replace-rules/%E5%8E%BB%E5%B9%BF%E5%91%8A"))
        .bearer_auth(&server.token)
        .json(&json!({ "pattern": "新广告.*" }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let body: Value = patched.json().await.unwrap();
    assert_eq!(body["pattern"], json!("新广告.*"));
    assert_eq!(body["replacement"], json!("[已移除]"), "未传字段保持不变");

    // 删除幂等：204 两次
    for _ in 0..2 {
        let deleted = server
            .client
            .delete(server.url("/v1/me/replace-rules/%E5%8E%BB%E5%B9%BF%E5%91%8A"))
            .bearer_auth(&server.token)
            .send()
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    }
    let list: Value = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.as_array().unwrap().is_empty());

    // PATCH 不存在的规则：404
    let missing = server
        .client
        .patch(server.url("/v1/me/replace-rules/%E4%B8%8D%E5%AD%98%E5%9C%A8"))
        .bearer_auth(&server.token)
        .json(&json!({ "pattern": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // 空 pattern：422 VALIDATION_FAILED 带 pointer
    let invalid = server
        .client
        .post(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .json(&json!({ "name": "坏规则", "pattern": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = invalid.json().await.unwrap();
    assert_eq!(body["code"], json!("VALIDATION_FAILED"));
    assert_eq!(body["errors"][0]["pointer"], json!("/name"));

    server.cleanup();
}

#[tokio::test]
async fn batch_upsert_and_delete() {
    let server = TestServer::start().await;

    let batch = server
        .client
        .post(server.url("/v1/me/replace-rules:batch"))
        .bearer_auth(&server.token)
        .json(&json!({
            "rules": [
                { "name": "甲", "pattern": "a" },
                { "name": "乙", "pattern": "b", "isRegex": true }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch.status(), StatusCode::OK);
    let body: Value = batch.json().await.unwrap();
    assert_eq!(body["count"], json!(2));

    let list: Value = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);

    let deleted = server
        .client
        .post(server.url("/v1/me/replace-rules:batchDelete"))
        .bearer_auth(&server.token)
        .json(&json!({ "names": ["甲", "乙", "不存在"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let body: Value = deleted.json().await.unwrap();
    assert_eq!(body["deleted"], json!(3));

    let list: Value = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.as_array().unwrap().is_empty());

    server.cleanup();
}
