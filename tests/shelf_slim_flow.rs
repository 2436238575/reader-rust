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
        .post(format!("{base_url}/v1/auth/login"))
        .header("x-requested-with", "XMLHttpRequest")
        .json(&json!({ "username": "reader1", "password": "password123" }))
        .send()
        .await
        .unwrap();
    let payload: Value = resp.json().await.unwrap();
    let token = payload["accessToken"].as_str().unwrap().to_string();
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

    async fn v1(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = self
            .client
            .request(method, format!("{}/v1{path}", self.base_url))
            .bearer_auth(&self.token)
            .header("x-requested-with", "XMLHttpRequest");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, value)
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

/// 兼容性：legacy `saveBooks`（裸数组、restore=false）历来忽略客户端
/// `bookId`；恢复专用的 `book_<n>` 格式校验不能收紧到普通整架替换。
#[tokio::test]
async fn legacy_save_books_ignores_client_book_id() {
    let server = boot().await;
    let saved = server
        .post(
            "saveBooks",
            json!([{
                "bookId": "uuid-123",
                "name": "兼容书",
                "author": "作者",
                "bookUrl": "https://shelf-slim.example/legacy-id"
            }]),
        )
        .await;
    assert_eq!(saved["isSuccess"], serde_json::json!(true));
    assert_ne!(
        saved["data"][0]["bookId"],
        serde_json::json!("uuid-123"),
        "非恢复语义应忽略客户端 bookId"
    );
}

#[tokio::test]
async fn v1_shelf_order_only_rewrites_positions_and_requires_full_set() {
    let server = boot().await;
    let book = |url: &str, name: &str| {
        json!({
            "name": name,
            "author": "作者",
            "bookUrl": url
        })
    };
    let a = server
        .post(
            "saveBook",
            book("https://shelf-slim.example/order-a", "排序甲"),
        )
        .await["data"]["bookId"]
        .as_str()
        .unwrap()
        .to_string();
    let b = server
        .post(
            "saveBook",
            book("https://shelf-slim.example/order-b", "排序乙"),
        )
        .await["data"]["bookId"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, problem) = server
        .v1(
            reqwest::Method::PATCH,
            "/me/shelf:order",
            Some(json!({ "bookIds": [b.clone(), a.clone()] })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{problem}");

    let (status, page) = server
        .v1(reqwest::Method::GET, "/me/shelf?limit=100", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["bookId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![b.as_str(), a.as_str()]);
    assert_eq!(page["items"][0]["position"], json!(1));
    assert_eq!(page["items"][1]["position"], json!(2));

    // 客户端视图过期（缺一本）→ 整批拒绝，顺序不动
    let (status, problem) = server
        .v1(
            reqwest::Method::PATCH,
            "/me/shelf:order",
            Some(json!({ "bookIds": [b.clone()] })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(
        problem["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("书架已变化"),
        "{problem}"
    );
    assert_eq!(problem["errors"][0]["pointer"], json!("/bookIds"));

    let (status, problem) = server
        .v1(
            reqwest::Method::PATCH,
            "/me/shelf:order",
            Some(json!({ "bookIds": [] })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["errors"][0]["pointer"], json!("/bookIds"));

    for body in [
        json!({ "bookIds": [a.clone(), a.clone()] }),
        json!({ "bookIds": ["book_bad"] }),
    ] {
        let (status, problem) = server
            .v1(reqwest::Method::PATCH, "/me/shelf:order", Some(body))
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        assert_eq!(problem["errors"][0]["pointer"], json!("/bookIds"));
    }

    let (status, problem) = server
        .v1(
            reqwest::Method::PATCH,
            "/me/shelf:order",
            Some(json!({ "bookIds": [a.clone()], "extra": 1 })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["code"], json!("UNKNOWN_FIELD"));
}
