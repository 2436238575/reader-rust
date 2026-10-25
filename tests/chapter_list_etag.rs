//! 目录接口的 ETag/304 再验证：
//! - 首次 GET 返回 200 + 强 ETag（md5）+ `Cache-Control: private, no-cache`；
//! - `If-None-Match` 命中（含 `W/` 弱前缀与逗号列表的宽松比较）返回 304 空 body；
//! - 目录内容变化后 ETag 改变，旧 ETag 不再 304（`refresh=1` 重抓路径）；
//! - 缓存命中路径与首屏新抓路径对同一目录产出同一 ETag。

mod common;
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 假上游目录的章节数：测试中途改动它来模拟「目录变了」。
type ChapterCount = Arc<AtomicUsize>;

async fn start_upstream(chapter_count: ChapterCount) -> String {
    let app = Router::new()
        .route(
            "/catalog",
            get(
                |State(chapter_count): State<ChapterCount>,
                 headers: axum::http::HeaderMap| async move {
                    let host = headers
                        .get("host")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let count = chapter_count.load(Ordering::SeqCst);
                    let items: Vec<Value> = (0..count)
                        .map(|i| {
                            json!({
                                "title": format!("第{}章", i + 1),
                                "url": format!("http://{host}/content?item_id={}", 100 + i),
                            })
                        })
                        .collect();
                    Json(json!({ "data": { "list": items } }))
                },
            ),
        )
        .with_state(chapter_count);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

struct TestServer {
    base_url: String,
    chapter_count: ChapterCount,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
    book_id: String,
}

impl TestServer {
    async fn start() -> Self {
        let chapter_count: ChapterCount = Arc::new(AtomicUsize::new(3));
        let upstream_url = start_upstream(chapter_count.clone()).await;

        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-toc-etag-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "toc-etag-test-secret".to_string(),
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
        let token = register(&client, &base_url).await;
        let book_source_id = save_source(&client, &base_url, &token, &upstream_url).await;
        let book_id = save_book(&client, &base_url, &token, &upstream_url, book_source_id).await;
        Self {
            base_url,
            chapter_count,
            temp_dir,
            client,
            token,
            book_id,
        }
    }

    /// GET 目录接口（真实 HTTP，浏览器再验证走的同一条路）。
    async fn get_chapter_list(
        &self,
        query: &str,
        if_none_match: Option<&str>,
    ) -> reqwest::Response {
        let mut request = self
            .client
            .get(format!("{}/reader3/getChapterList?{query}", self.base_url))
            .bearer_auth(&self.token);
        if let Some(value) = if_none_match {
            request = request.header("If-None-Match", value);
        }
        request.send().await.unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

async fn register(client: &reqwest::Client, base_url: &str) -> String {
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

/// 保存假书源（目录规则只解析 `$.data.list[*]`），返回分配到的 bookSourceId。
async fn save_source(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    upstream_url: &str,
) -> i64 {
    let source = json!({
        "bookSourceName": "目录ETag假源",
        "bookSourceUrl": upstream_url,
        "concurrentRate": "0",
        "enabled": true,
        "ruleToc": {
            "chapterList": "$.data.list[*]",
            "chapterName": "$.title",
            "chapterUrl": "$.url"
        }
    });
    let resp = client
        .post(format!("{base_url}/reader3/saveBookSource"))
        .bearer_auth(token)
        .json(&source)
        .send()
        .await
        .unwrap();
    let payload: Value = resp.json().await.unwrap();
    assert_eq!(
        payload["isSuccess"],
        json!(true),
        "保存书源应成功: {payload}"
    );
    payload["data"]["bookSourceId"].as_i64().unwrap()
}

async fn save_book(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    upstream_url: &str,
    book_source_id: i64,
) -> String {
    let book = json!({
        "name": "测试书",
        "author": "作者",
        "bookUrl": format!("{upstream_url}/info?book_id=1"),
        "bookSourceId": book_source_id,
        "tocUrl": format!("{upstream_url}/catalog?book_id=1"),
    });
    let resp = client
        .post(format!("{base_url}/reader3/saveBook"))
        .bearer_auth(token)
        .json(&book)
        .send()
        .await
        .unwrap_or_else(|e| panic!("saveBook 请求失败: {e}"));
    let payload: Value = resp.json().await.unwrap();
    assert_eq!(payload["isSuccess"], json!(true), "入架应成功: {payload}");
    payload["data"]["bookId"].as_str().unwrap().to_string()
}

fn etag_of(resp: &reqwest::Response) -> String {
    resp.headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// 首次 200 拿 ETag；If-None-Match 命中（含弱前缀/列表）拿 304 空 body；未命中回 200。
#[tokio::test]
async fn chapter_list_serves_etag_and_304_revalidation() {
    let server = TestServer::start().await;
    let query = format!("bookId={}", server.book_id);

    // 首次 GET（首屏新抓路径）：200 + 强 ETag + private, no-cache
    let resp = server.get_chapter_list(&query, None).await;
    assert_eq!(resp.status(), StatusCode::OK, "首次请求应返回 200");
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
        "private, no-cache",
        "必须声明私有缓存且每次再验证"
    );
    let etag = etag_of(&resp);
    assert_eq!(
        etag.len(),
        34,
        "强 ETag 应是带引号的 md5 hex（32 位 + 两侧引号）: {etag}"
    );
    assert!(
        etag.starts_with('"') && etag.ends_with('"'),
        "ETag 必须带引号: {etag}"
    );
    let payload: Value = resp.json().await.unwrap();
    assert_eq!(payload["isSuccess"], json!(true));
    assert_eq!(payload["data"].as_array().unwrap().len(), 3);

    // 缓存命中路径：同 ETag 再验证 → 304 空 body，ETag/Cache-Control 头照带
    let resp = server.get_chapter_list(&query, Some(&etag)).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_MODIFIED,
        "未变化的目录应 304"
    );
    assert_eq!(etag_of(&resp), etag, "304 的 ETag 应与 200 一致");
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
        "private, no-cache"
    );
    let body = resp.bytes().await.unwrap();
    assert!(body.is_empty(), "304 body 必须为空，实际: {body:?}");

    // W/ 弱前缀 + 逗号列表的宽松比较也算命中
    let weak_list = format!("\"stale-etag\", W/{etag}");
    let resp = server.get_chapter_list(&query, Some(&weak_list)).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_MODIFIED,
        "弱前缀与列表中的匹配项应命中: {weak_list}"
    );

    // 不匹配的 ETag → 200 完整 JSON
    let resp = server
        .get_chapter_list(&query, Some("\"00000000000000000000000000000000\""))
        .await;
    assert_eq!(resp.status(), StatusCode::OK, "不匹配的 ETag 应回 200");
    assert_eq!(etag_of(&resp), etag);
    let payload: Value = resp.json().await.unwrap();
    assert_eq!(payload["data"].as_array().unwrap().len(), 3);
}

/// 目录内容变化（上游新增一章 + refresh=1 重抓）后 ETag 改变，旧 ETag 不再 304。
#[tokio::test]
async fn etag_changes_when_toc_content_changes() {
    let server = TestServer::start().await;

    let resp = server
        .get_chapter_list(&format!("bookId={}", server.book_id), None)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let etag_v1 = etag_of(&resp);
    assert!(!etag_v1.is_empty(), "首次响应必须带 ETag");

    // 上游目录多了一章；带旧 ETag 强刷重抓（首屏新抓路径）
    server.chapter_count.store(4, Ordering::SeqCst);
    let resp = server
        .get_chapter_list(
            &format!("bookId={}&refresh=1", server.book_id),
            Some(&etag_v1),
        )
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "目录已变化，旧 ETag 不应再命中 304"
    );
    let etag_v2 = etag_of(&resp);
    assert_ne!(etag_v1, etag_v2, "目录变化必须产生新 ETag");
    let payload: Value = resp.json().await.unwrap();
    assert_eq!(payload["data"].as_array().unwrap().len(), 4);

    // 新 ETag 在缓存命中路径上 304
    let resp = server
        .get_chapter_list(&format!("bookId={}", server.book_id), Some(&etag_v2))
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_MODIFIED,
        "新目录的新 ETag 应命中"
    );
}
