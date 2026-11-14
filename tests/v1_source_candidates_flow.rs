//! `/v1/books/{bookId}/source-candidates/events`：换源候选 SSE 的接线与参数校验。
//!
//! 这里不构造真实上游（候选命中逻辑由 legacy 共用实现承担）；用例覆盖
//! 路由/鉴权/SSE 帧/参数校验/404，确保前端从 `/reader3` 切过来后行为不变。

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
        let temp_dir = std::env::temp_dir().join(format!(
            "reader-rust-v1-source-candidates-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = common::base_config(&temp_dir, "v1-source-candidates-secret");
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

    async fn local_book(&self, filename: &str) -> String {
        let boundary = "----source-candidates-test";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: text/plain\r\n\r\n第一章 开始\n正文。\r\n--{boundary}--\r\n"
        )
        .into_bytes();
        let response = self
            .client
            .post(format!("{}/v1/local-books", self.base_url))
            .bearer_auth(&self.token)
            .header("x-requested-with", "XMLHttpRequest")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        body["bookId"].as_str().unwrap().to_string()
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn source_candidate_sse_emits_end_when_no_sources() {
    let server = TestServer::start().await;
    let book_id = server.local_book("候选目标书.txt").await;

    let response = server
        .get(&format!(
            "/v1/books/{book_id}/source-candidates/events?lastIndex=-1&concurrentCount=8"
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream")),
        "必须是 SSE 响应"
    );
    let body = response.text().await.unwrap();
    assert!(body.contains("event: end"), "{body}");
    assert!(body.contains("\"hasMore\":false"), "{body}");

    server.cleanup();
}

#[tokio::test]
async fn source_candidate_sse_validates_book_and_query() {
    let server = TestServer::start().await;

    let response = server
        .get("/v1/books/book_999999/source-candidates/events")
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let problem: Value = response.json().await.unwrap();
    assert_eq!(problem["code"], json!("BOOK_NOT_FOUND"));

    let book_id = server.local_book("候选查询书.txt").await;
    for (query, pointer) in [
        ("lastIndex=-2", "/lastIndex"),
        ("concurrentCount=0", "/concurrentCount"),
        ("concurrentCount=21", "/concurrentCount"),
    ] {
        let response = server
            .get(&format!(
                "/v1/books/{book_id}/source-candidates/events?{query}"
            ))
            .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let problem: Value = response.json().await.unwrap();
        assert_eq!(problem["code"], json!("VALIDATION_FAILED"));
        assert_eq!(problem["errors"][0]["pointer"], json!(pointer), "{problem}");
    }

    server.cleanup();
}
