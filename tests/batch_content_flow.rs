//! 批量正文接口 getBookContents（预加载用）的端到端行为：
//! - 多章成功：每章一条 data，按请求顺序返回；
//! - 单章失败（上游 403）只写该条 error，其余章节照常成功，整体 200；
//! - chapters 为空或超过 10 条整批 400；
//! - 单章语义与 getBookContent 一致（共用同一条读取链路）。

mod common;
use axum::{http::StatusCode, response::Response, routing::get, Json, Router};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use serde_json::{json, Value};
use std::path::PathBuf;

/// 假上游：/c1、/c2 正常正文，/missing 返回 403（单章失败用例）。
async fn start_upstream() -> String {
    let app = Router::new()
        .route(
            "/c1",
            get(|| async { Json(json!({ "data": { "content": "第一章正文" } })) }),
        )
        .route(
            "/c2",
            get(|| async { Json(json!({ "data": { "content": "第二章正文" } })) }),
        )
        .route(
            "/missing",
            get(|| async {
                let mut resp = Response::new(axum::body::Body::from("blocked"));
                *resp.status_mut() = StatusCode::FORBIDDEN;
                resp
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn plain_source() -> Value {
    json!({
        "bookSourceName": "假书源",
        "enabled": true,
        "ruleContent": { "content": "$.data.content" }
    })
}

struct TestServer {
    base_url: String,
    upstream_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
    book_id: String,
}

impl TestServer {
    async fn start() -> Self {
        let upstream_url = start_upstream().await;
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-batch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "batch-content-test-secret".to_string(),
            request_timeout_secs: 10,
            admin_username: "reader1".to_string(),
            admin_password: "password123".to_string(),
            rate_limit_disabled: true,
            // 假上游监听 127.0.0.1：显式放行私网出站（默认拦截）
            allow_private_network: true,
            ..AppConfig::default()
        };

        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
        });

        let client = reqwest::Client::new();
        let base_url = format!("http://{addr}");
        let token = login(&client, &base_url).await;
        let mut server = Self {
            base_url,
            upstream_url,
            temp_dir,
            client,
            token,
            book_id: String::new(),
        };

        let mut source = plain_source();
        source["bookSourceUrl"] = json!(server.upstream_url);
        server.save_source(&source).await;
        server.register_book().await;
        server
    }

    async fn save_source(&self, source: &Value) {
        let resp = self
            .client
            .post(format!("{}/reader3/saveBookSource", self.base_url))
            .bearer_auth(&self.token)
            .json(source)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    async fn register_book(&mut self) {
        let resp = self
            .client
            .get(format!("{}/reader3/getBookSources", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        let source_id = payload["data"][0]["bookSourceId"].as_i64().unwrap();
        let book = json!({
            "name": "测试书",
            "author": "作者",
            "bookUrl": format!("{}/c1", self.upstream_url),
            "bookSourceId": source_id,
        });
        let resp = self
            .client
            .post(format!("{}/reader3/saveBook", self.base_url))
            .bearer_auth(&self.token)
            .json(&book)
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        self.book_id = payload["data"]["bookId"].as_str().unwrap().to_string();
    }

    /// 批量取正文：返回 (HTTP 状态, 响应 JSON)
    async fn get_contents(&self, body: &Value) -> (StatusCode, Value) {
        let resp = self
            .client
            .post(format!("{}/reader3/getBookContents", self.base_url))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let payload: Value = resp.json().await.unwrap();
        (status, payload)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

async fn login(client: &reqwest::Client, base_url: &str) -> String {
    let resp = client
        .post(format!("{base_url}/reader3/login"))
        .json(&json!({ "username": "reader1", "password": "password123" }))
        .send()
        .await
        .unwrap();
    let payload: Value = resp.json().await.unwrap();
    assert_eq!(payload["isSuccess"], json!(true), "登录应成功: {payload}");
    payload["data"]["accessToken"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn two_chapters_return_two_data_items_in_request_order() {
    let server = TestServer::start().await;
    let body = json!({
        "bookId": server.book_id,
        "chapters": [
            { "index": 1, "chapterUrl": format!("{}/c2", server.upstream_url) },
            { "index": 0, "chapterUrl": format!("{}/c1", server.upstream_url) },
        ]
    });

    let (status, payload) = server.get_contents(&body).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["isSuccess"], json!(true), "{payload}");
    let chapters = payload["data"].as_array().unwrap();
    assert_eq!(chapters.len(), 2);
    // 按请求顺序回排（不是完成顺序），前端按下标对号入座
    assert_eq!(chapters[0]["index"], json!(1));
    assert_eq!(chapters[1]["index"], json!(0));
    assert_eq!(chapters[0]["error"], json!(null));
    assert_eq!(chapters[1]["error"], json!(null));
    assert_eq!(chapters[0]["data"]["content"], json!("第二章正文"));
    assert_eq!(chapters[1]["data"]["content"], json!("第一章正文"));
}

#[tokio::test]
async fn single_chapter_failure_does_not_affect_others() {
    let server = TestServer::start().await;
    let body = json!({
        "bookId": server.book_id,
        "chapters": [
            { "index": 0, "chapterUrl": format!("{}/missing", server.upstream_url) },
            { "index": 1, "chapterUrl": format!("{}/c1", server.upstream_url) },
        ]
    });

    let (status, payload) = server.get_contents(&body).await;

    // 单章失败不影响其他章节：整体仍然 200
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["isSuccess"], json!(true), "{payload}");
    let chapters = payload["data"].as_array().unwrap();
    assert_eq!(chapters.len(), 2);
    // 失败章节：data 为 null，error 带真实原因（上游 403）
    assert_eq!(chapters[0]["data"], json!(null));
    let error = chapters[0]["error"].as_str().unwrap();
    assert!(!error.is_empty(), "失败章节应给出原因: {payload}");
    assert!(error.contains("403"), "error 应带上游状态码: {error}");
    // 成功章节照常返回
    assert_eq!(chapters[1]["error"], json!(null));
    assert_eq!(chapters[1]["data"]["content"], json!("第一章正文"));
}

#[tokio::test]
async fn empty_chapters_is_rejected_with_400() {
    let server = TestServer::start().await;
    let body = json!({ "bookId": server.book_id, "chapters": [] });

    let (status, payload) = server.get_contents(&body).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(payload["isSuccess"], json!(false), "{payload}");
    assert!(
        !payload["errorMsg"].as_str().unwrap().is_empty(),
        "应说明 chapters 数量限制: {payload}"
    );
}

#[tokio::test]
async fn eleven_chapters_is_rejected_with_400() {
    let server = TestServer::start().await;
    let chapters: Vec<Value> = (0..11)
        .map(|i| json!({ "index": i, "chapterUrl": format!("{}/c1", server.upstream_url) }))
        .collect();
    let body = json!({ "bookId": server.book_id, "chapters": chapters });

    let (status, payload) = server.get_contents(&body).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(payload["isSuccess"], json!(false), "{payload}");
}

#[tokio::test]
async fn missing_chapter_url_is_rejected_with_400() {
    let server = TestServer::start().await;
    let body = json!({
        "bookId": server.book_id,
        "chapters": [
            { "index": 0, "chapterUrl": format!("{}/c1", server.upstream_url) },
            { "index": 1 },
        ]
    });

    let (status, payload) = server.get_contents(&body).await;

    // chapterUrl 必填：缺了整批拒绝，不做半执行
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(payload["isSuccess"], json!(false), "{payload}");
}

#[tokio::test]
async fn batch_data_matches_single_chapter_endpoint() {
    // 批量与单章共用同一条读取链路：同章的 content 必须一致
    let server = TestServer::start().await;
    let chapter_url = format!("{}/c1", server.upstream_url);

    let single = server
        .client
        .post(format!("{}/reader3/getBookContent", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "bookId": server.book_id, "chapterUrl": chapter_url }))
        .send()
        .await
        .unwrap();
    assert_eq!(single.status(), StatusCode::OK);
    let single_payload: Value = single.json().await.unwrap();

    let (status, payload) = server
        .get_contents(&json!({
            "bookId": server.book_id,
            "chapters": [ { "index": 0, "chapterUrl": chapter_url } ]
        }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let item = &payload["data"][0];
    assert_eq!(item["error"], json!(null));
    assert_eq!(
        item["data"]["content"], single_payload["data"]["content"],
        "批量与单章的正文应一致: {payload} vs {single_payload}"
    );
    // 响应结构对齐单章：关键字段都在
    for key in ["edited", "variant", "stale", "selectionSource"] {
        assert!(
            item["data"].get(key).is_some(),
            "批量 data 应带单章字段 {key}: {payload}"
        );
    }
}
