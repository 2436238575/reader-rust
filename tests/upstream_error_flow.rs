//! 上游错误透传的端到端行为：
//! - 上游 4xx/5xx 不再被当成「成功但空」返回，而是 502 + 带状态码的可读文案；
//! - 2xx 但正文解析为空：书源声明了配图规则（纯图章节）放行，未声明报错。

use axum::{http::StatusCode, response::Response, routing::get, Json, Router};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use serde_json::{json, Value};
use std::path::PathBuf;

/// 假上游：/ok 正常正文、/missing 返回 403、/empty 返回 200 但正文为空、
/// /info 返回目录（cacheBookSSE 用例需要）。
async fn start_upstream() -> String {
    let app = Router::new()
        .route(
            "/ok",
            get(|| async { Json(json!({ "data": { "content": "第一段正文" } })) }),
        )
        .route(
            "/missing",
            get(|| async {
                let mut resp = Response::new(axum::body::Body::from("blocked"));
                *resp.status_mut() = StatusCode::FORBIDDEN;
                resp
            }),
        )
        .route(
            "/empty",
            get(|| async { Json(json!({ "data": { "content": "" } })) }),
        )
        .route(
            "/info",
            get(|| async {
                Json(json!({ "data": { "chapters": [{ "title": "第1章", "url": "/ok" }] } }))
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// 无配图规则的普通书源（带目录规则，cacheBookSSE 用例要用）。
fn plain_source(upstream_url: &str) -> Value {
    json!({
        "bookSourceName": "假书源",
        "bookSourceUrl": upstream_url,
        "concurrentRate": "0",
        "enabled": true,
        "ruleToc": {
            "chapterList": "$.data.chapters[*]",
            "chapterName": "$.title",
            "chapterUrl": "$.url"
        },
        "ruleContent": { "content": "$.data.content" }
    })
}

/// 带配图规则的书源（空正文 = 纯图章节，合法）。
fn image_source(upstream_url: &str) -> Value {
    json!({
        "bookSourceName": "假图源",
        "bookSourceUrl": upstream_url,
        "concurrentRate": "0",
        "enabled": true,
        "ruleContent": { "content": "$.data.content" },
        "ruleContentImage": {
            "imageUrl": "content/image?item_id=1",
            "listRule": "$.data.images[*]"
        }
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
    async fn start(source: Value) -> Self {
        let upstream_url = start_upstream().await;
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-upstream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "upstream-test-secret".to_string(),
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
        let mut source = source;
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
        let source_id = self.source_id().await;
        let book = json!({
            "name": "测试书",
            "author": "作者",
            "bookUrl": format!("{}/info?book_id=1", self.upstream_url),
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

    async fn source_id(&self) -> i64 {
        let resp = self
            .client
            .get(format!("{}/reader3/getBookSources", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        payload["data"][0]["bookSourceId"].as_i64().unwrap()
    }

    /// 取正文：返回 (HTTP 状态, isSuccess, errorMsg, data)
    async fn get_content(&self, chapter_url: &str) -> (StatusCode, Value) {
        let resp = self
            .client
            .post(format!("{}/reader3/getBookContent", self.base_url))
            .bearer_auth(&self.token)
            .json(&json!({ "bookId": self.book_id, "chapterUrl": chapter_url }))
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
        let _ = std::fs::remove_dir_all(&self.temp_dir);
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
async fn upstream_403_is_reported_with_status_code() {
    let server = TestServer::start(plain_source("")).await;
    let chapter_url = format!("{}/missing", server.upstream_url);

    let (status, payload) = server.get_content(&chapter_url).await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(payload["isSuccess"], json!(false));
    let msg = payload["errorMsg"].as_str().unwrap();
    assert!(msg.contains("403"), "errorMsg 应带真实状态码: {msg}");
}

#[tokio::test]
async fn empty_content_without_image_rule_is_an_error() {
    let server = TestServer::start(plain_source("")).await;
    let chapter_url = format!("{}/empty", server.upstream_url);

    let (status, payload) = server.get_content(&chapter_url).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(payload["isSuccess"], json!(false));
    assert!(
        payload["errorMsg"].as_str().unwrap().contains("正文为空"),
        "应说明正文为空: {payload}"
    );
}

#[tokio::test]
async fn empty_content_with_image_rule_stays_successful() {
    // 纯图章节：书源声明了配图规则，正文为空是合法形态（图片走配图接口）
    let server = TestServer::start(image_source("")).await;
    let chapter_url = format!("{}/empty", server.upstream_url);

    let (status, payload) = server.get_content(&chapter_url).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["isSuccess"], json!(true));
    assert_eq!(payload["data"], json!(""));
}

#[tokio::test]
async fn sse_responses_disable_proxy_buffering() {
    // 经 nginx 反代时默认 proxy_buffering 会把事件流攒到结束才转发
    //（直连正常、反代「进度一下走完」）；SSE 响应必须带这个头
    let server = TestServer::start(plain_source("")).await;
    let resp = server
        .client
        .get(format!("{}/reader3/cacheBookSSE", server.base_url))
        .bearer_auth(&server.token)
        .query(&[("bookId", server.book_id.as_str()), ("count", "1")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-accel-buffering")
            .and_then(|v| v.to_str().ok()),
        Some("no")
    );
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream")),
        "应当是 SSE 响应"
    );
    // 不消费流：drop 即断开
}

#[tokio::test]
async fn normal_content_still_works() {
    let server = TestServer::start(plain_source("")).await;
    let chapter_url = format!("{}/ok", server.upstream_url);

    let (status, payload) = server.get_content(&chapter_url).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["data"], json!("第一段正文"));
}
