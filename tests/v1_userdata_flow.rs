//! `PUT /v1/me/userdata:batch`：批量整文档覆盖写的校验与落库。

mod common;

use axum::http::StatusCode;
use serde_json::{json, Value};
use std::path::PathBuf;

use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;

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
            std::env::temp_dir().join(format!("reader-rust-v1-userdata-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = common::base_config(&temp_dir, "v1-userdata-secret");
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

    async fn save(&self, body: Value) -> (StatusCode, Value) {
        let response = self
            .client
            .put(format!("{}/v1/me/userdata:batch", self.base_url))
            .bearer_auth(&self.token)
            .header("x-requested-with", "XMLHttpRequest")
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let payload: Value = response.json().await.unwrap();
        (status, payload)
    }

    async fn read_legacy(&self, names: &str) -> Value {
        let response = self
            .client
            .get(format!(
                "{}/reader3/getUserdataBatch?names={names}",
                self.base_url
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
    }
}

#[tokio::test]
async fn userdata_batch_saves_all_docs_and_last_write_wins() {
    let server = TestServer::start().await;
    let (status, payload) = server
        .save(json!({
            "docs": [
                { "name": "reading-stats", "value": { "totalSeconds": 1 } },
                { "name": "recent-books", "value": [{ "bookId": "book_1" }] }
            ]
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{payload}");
    assert_eq!(payload["saved"], json!(2));

    let docs = server.read_legacy("reading-stats,recent-books").await;
    assert_eq!(
        docs["data"]["docs"]["reading-stats"]["totalSeconds"],
        json!(1)
    );
    assert_eq!(
        docs["data"]["docs"]["recent-books"][0]["bookId"],
        json!("book_1")
    );

    // 同名文档按最后一次为准
    let (status, payload) = server
        .save(json!({
            "docs": [
                { "name": "reading-stats", "value": { "totalSeconds": 2 } },
                { "name": "reading-stats", "value": { "totalSeconds": 3 } }
            ]
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{payload}");
    assert_eq!(payload["saved"], json!(1));
    let docs = server.read_legacy("reading-stats").await;
    assert_eq!(
        docs["data"]["docs"]["reading-stats"]["totalSeconds"],
        json!(3)
    );

    server.cleanup();
}

#[tokio::test]
async fn userdata_batch_rejects_invalid_docs_before_writing() {
    let server = TestServer::start().await;

    let cases = [
        (json!({ "docs": [] }), "/docs"),
        (
            json!({ "docs": [{ "name": "../evil", "value": 1 }] }),
            "/docs/0/name",
        ),
        (json!({ "docs": [{ "name": "ok-doc" }] }), "/docs/0/value"),
    ];
    for (body, pointer) in cases {
        let (status, problem) = server.save(body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        assert_eq!(problem["code"], json!("VALIDATION_FAILED"));
        assert_eq!(problem["errors"][0]["pointer"], json!(pointer), "{problem}");
    }

    let too_many: Vec<Value> = (0..21)
        .map(|index| json!({ "name": format!("doc-{index}"), "value": index }))
        .collect();
    let (status, problem) = server.save(json!({ "docs": too_many })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["errors"][0]["pointer"], json!("/docs"));

    // 非法批次一个都不落库
    let docs = server.read_legacy("ok-doc").await;
    assert_eq!(docs["data"]["docs"]["ok-doc"], Value::Null);

    // 合法 + 非法混合：整批拒绝，合法条目也不落库
    let (status, problem) = server
        .save(json!({
            "docs": [
                { "name": "atomic-ok", "value": { "ok": true } },
                { "name": "../bad", "value": 2 }
            ]
        }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    let docs = server.read_legacy("atomic-ok").await;
    assert_eq!(docs["data"]["docs"]["atomic-ok"], Value::Null);

    server.cleanup();
}

#[tokio::test]
async fn userdata_batch_accepts_over_1mb_and_rejects_doc_over_4mb() {
    let server = TestServer::start().await;

    // v1 外层默认 1MB；路由级 8MB 必须真实生效
    let large = "x".repeat(1_200_000);
    let (status, payload) = server
        .save(json!({ "docs": [{ "name": "large-doc", "value": large }] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{payload}");
    assert_eq!(payload["saved"], json!(1));

    // 单文档序列化后超过 4MB → 413（请求体仍小于 8MB）
    let too_large = "y".repeat(4 * 1024 * 1024 + 1024);
    let (status, problem) = server
        .save(json!({ "docs": [{ "name": "too-large-doc", "value": too_large }] }))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{problem}");

    server.cleanup();
}
