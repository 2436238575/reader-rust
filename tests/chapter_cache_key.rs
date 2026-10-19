//! 正文缓存的落盘位置：getBookContent 必须把正文缓存到 `cache/<ns>/<bookId>/` 目录下。
//!
//! bookId 是唯一身份参数：曾经「只带 chapterUrl 靠书架/目录缓存反推 bookUrl」的
//! 兜底链路已删除（API 型书源的书/章节 URL 没有公共前缀，反推失败会把缓存写进
//! md5(chapterUrl) 孤儿目录）。现在请求缺 bookId 直接 400，孤儿目录无从产生。

use axum::{extract::Query, http::StatusCode, routing::get, Json, Router};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use reader_rust::util::hash::md5_hex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

type HitCounter = Arc<Mutex<HashMap<String, usize>>>;

/// 假上游：复刻番茄Web 的 API 形态——书 /info、目录 /catalog、正文 /content
/// 三个端点没有公共路径前缀。统计每个路径被请求的次数。
async fn start_upstream() -> (String, HitCounter) {
    let hits: HitCounter = Arc::new(Mutex::new(HashMap::new()));

    let app = Router::new()
        .route(
            "/catalog",
            get(|headers: axum::http::HeaderMap| async move {
                let host = headers
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let items: Vec<Value> = (0..3)
                    .map(|i| {
                        json!({
                            "title": format!("第{}章", i + 1),
                            "url": format!("http://{host}/content?item_id={}", 100 + i),
                        })
                    })
                    .collect();
                Json(json!({ "data": { "list": items } }))
            }),
        )
        .route(
            "/content",
            get(|Query(params): Query<HashMap<String, String>>| async move {
                let item = params.get("item_id").cloned().unwrap_or_default();
                Json(json!({ "data": { "data": { "content": format!("章节 {item} 的正文") } } }))
            }),
        )
        .route(
            "/__hits",
            get(
                |axum::extract::State(hits): axum::extract::State<HitCounter>| async move {
                    let snapshot: HashMap<String, usize> = hits.lock().unwrap().clone();
                    Json(serde_json::to_value(snapshot).unwrap())
                },
            ),
        )
        .with_state(hits.clone());

    let counter = hits.clone();
    let app = app.layer(axum::middleware::from_fn(
        move |request: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| {
            let counter = counter.clone();
            async move {
                let path = request.uri().path().to_string();
                *counter.lock().unwrap().entry(path).or_insert(0) += 1;
                next.run(request).await
            }
        },
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), hits)
}

struct TestServer {
    base_url: String,
    upstream_url: String,
    hits: HitCounter,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
    book_source_id: i64,
    book_id: String,
}

fn book_source(upstream_url: &str) -> Value {
    json!({
        "bookSourceName": "API型假源",
        "bookSourceUrl": upstream_url,
        "concurrentRate": "0",
        "enabled": true,
        "ruleContent": { "content": "$.data.data.content" },
        "ruleToc": {
            "chapterList": "$.data.list[*]",
            "chapterName": "$.title",
            "chapterUrl": "$.url"
        }
    })
}

impl TestServer {
    async fn start() -> Self {
        let (upstream_url, hits) = start_upstream().await;

        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-cache-key-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "cache-key-test-secret".to_string(),
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
        let token = register(&client, &base_url, "reader1").await;
        let book_source_id = save_source(&client, &base_url, &token, &upstream_url).await;
        let mut server = Self {
            base_url,
            upstream_url,
            hits,
            temp_dir,
            client,
            token,
            book_source_id,
            book_id: String::new(),
        };
        server.book_id = server.save_book().await;
        server
    }

    /// 书架塞一本书：bookUrl 与章节 URL 没有公共前缀（API 型书源）。
    /// 返回分配到的 bookId。
    async fn save_book(&self) -> String {
        let book = json!({
            "name": "测试书",
            "author": "作者",
            "bookUrl": self.book_url(),
            "bookSourceId": self.book_source_id,
            "tocUrl": self.toc_url(),
        });
        let resp = self
            .client
            .post(format!("{}/reader3/saveBook", self.base_url))
            .bearer_auth(&self.token)
            .json(&book)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let payload: Value = resp.json().await.unwrap();
        payload["data"]["bookId"].as_str().unwrap().to_string()
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
        let payload: Value = resp.json().await.unwrap();
        assert_eq!(
            payload["isSuccess"],
            json!(true),
            "{path} 应成功: {payload}"
        );
        payload["data"].clone()
    }

    fn book_url(&self) -> String {
        format!("{}/info?book_id=1", self.upstream_url)
    }

    fn toc_url(&self) -> String {
        format!("{}/catalog?book_id=1", self.upstream_url)
    }

    fn chapter_url(&self, index: usize) -> String {
        format!("{}/content?item_id={}", self.upstream_url, 100 + index)
    }

    /// 正文缓存应当落到的文件：`cache/<ns>/<bookId>/<md5(chapterUrl)>.txt`
    fn book_cache_file(&self, chapter_url: &str) -> PathBuf {
        self.temp_dir
            .join("cache")
            .join("user_1")
            .join(&self.book_id)
            .join(format!("{}.txt", md5_hex(chapter_url)))
    }

    fn hits_for(&self, path: &str) -> usize {
        *self.hits.lock().unwrap().get(path).unwrap_or(&0)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

async fn register(client: &reqwest::Client, base_url: &str, username: &str) -> String {
    let resp = client
        .post(format!("{base_url}/reader3/login"))
        .json(&json!({
            "username": username,
            "password": "password123",
        }))
        .send()
        .await
        .unwrap();
    let payload: Value = resp.json().await.unwrap();
    assert_eq!(payload["isSuccess"], json!(true), "登录应成功: {payload}");
    payload["data"]["accessToken"].as_str().unwrap().to_string()
}

/// 保存假书源，返回分配到的 bookSourceId。
async fn save_source(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    upstream_url: &str,
) -> i64 {
    let resp = client
        .post(format!("{base_url}/reader3/saveBookSource"))
        .bearer_auth(token)
        .json(&book_source(upstream_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let payload: Value = resp.json().await.unwrap();
    payload["data"]["bookSourceId"].as_i64().unwrap()
}

/// 主路径：请求带 bookId，正文直接缓存到 `cache/<ns>/<bookId>/` 目录。
#[tokio::test]
async fn content_with_explicit_book_url_lands_in_the_book_cache_dir() {
    let server = TestServer::start().await;

    let body = json!({
        "bookId": server.book_id,
        "chapterUrl": server.chapter_url(0),
    });
    let content = server.post("getBookContent", body.clone()).await;
    assert_eq!(content, json!("章节 100 的正文"));

    let file = server.book_cache_file(&server.chapter_url(0));
    assert!(
        file.exists(),
        "正文应缓存到 cache/<ns>/<bookId>/ 目录: {}",
        file.display()
    );

    server.post("getBookContent", body).await;
    assert_eq!(
        server.hits_for("/content"),
        1,
        "第二次应命中缓存，不再打上游"
    );
}

/// 缺 bookId 的请求直接 400：反推链路已删除，孤儿缓存目录无从产生。
#[tokio::test]
async fn content_without_book_id_is_rejected() {
    let server = TestServer::start().await;

    let resp = server
        .client
        .post(format!("{}/reader3/getBookContent", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "chapterUrl": server.chapter_url(0) }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "缺 bookId 应被拒绝");
    let payload: Value = resp.json().await.unwrap();
    assert!(
        payload["errorMsg"]
            .as_str()
            .unwrap_or_default()
            .contains("bookId"),
        "应报缺 bookId: {payload}"
    );

    // 缓存目录里不该出现任何以章节 URL 命名的孤儿目录
    let orphan = server
        .temp_dir
        .join("cache")
        .join("user_1")
        .join(md5_hex(&server.chapter_url(0)));
    assert!(
        !orphan.exists(),
        "不该产生孤儿缓存目录: {}",
        orphan.display()
    );
}
