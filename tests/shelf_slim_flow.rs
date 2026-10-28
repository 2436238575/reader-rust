//! 书架单书缓存章数注入的端到端行为：
//! - `getShelfBook`（单书）响应带 `cachedChapterCount`，阅读页缓存面板不必为此拉整架；
//! - `getShelfBookWithCacheInfo`（列表展示）延续既有契约：窄列拆分不带
//!   infoHtml/tocHtml 大快照（服务层 SHELF_LIST_COLS 保证），且每本注入缓存章数。

mod common;

use axum::http::StatusCode;
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use serde_json::{json, Value};
use std::path::PathBuf;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

async fn boot() -> TestServer {
    common::clean_stale_temp_dirs();
    let temp_dir =
        std::env::temp_dir().join(format!("reader-rust-shelf-slim-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let cfg = AppConfig {
        server_host: "127.0.0.1".to_string(),
        server_port: 0,
        database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
        storage_dir: temp_dir.to_string_lossy().to_string(),
        web_root: temp_dir.to_string_lossy().to_string(),
        assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
        jwt_secret: "shelf-slim-secret".to_string(),
        request_timeout_secs: 10,
        admin_username: "reader1".to_string(),
        admin_password: "password123".to_string(),
        rate_limit_disabled: true,
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
    let resp = client
        .post(format!("{base_url}/reader3/login"))
        .json(&json!({ "username": "reader1", "password": "password123" }))
        .send()
        .await
        .unwrap();
    let payload: Value = resp.json().await.unwrap();
    let token = payload["data"]["accessToken"].as_str().unwrap().to_string();
    TestServer {
        base_url,
        temp_dir,
        client,
        token,
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

impl TestServer {
    async fn get(&self, path: &str) -> Value {
        let resp = self
            .client
            .get(format!("{}/reader3/{path}", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        resp.json().await.unwrap()
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let resp = self
            .client
            .post(format!("{}/reader3/{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path} 应返回 200");
        resp.json().await.unwrap()
    }
}

#[tokio::test]
async fn single_shelf_book_reports_cached_chapter_count() {
    let server = boot().await;
    let saved = server
        .post(
            "saveBook",
            json!({
                "name": "计数书",
                "author": "作者",
                "bookUrl": "https://shelf-slim.example/info",
                "intro": "简介保持原样"
            }),
        )
        .await;
    let book_id = saved["data"]["bookId"].as_str().unwrap().to_string();

    // 展示列表契约：窄列（无 infoHtml/tocHtml 大快照）+ 缓存章数注入
    let list = server.get("getShelfBookWithCacheInfo").await;
    let entry = &list["data"][0];
    assert_eq!(entry["name"], "计数书");
    assert!(entry["infoHtml"].is_null(), "展示列表不应携带 infoHtml 值");
    assert!(entry["tocHtml"].is_null(), "展示列表不应携带 tocHtml 值");
    assert_eq!(entry["intro"], "简介保持原样");
    assert!(entry["cachedChapterCount"].is_i64(), "列表必须注入缓存章数");

    // 单书：cachedChapterCount 必在（无目录缓存时为 0），缓存面板据此单书查询
    let one = server
        .post("getShelfBook", json!({ "bookId": book_id }))
        .await;
    assert_eq!(one["data"]["bookId"], book_id);
    assert_eq!(one["data"]["cachedChapterCount"], 0);
    assert!(one["data"]["cachedChapterCount"].is_i64());
}
