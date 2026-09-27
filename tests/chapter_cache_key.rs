//! 正文缓存的落盘位置：getBookContent 必须把正文缓存到 md5(bookUrl) 目录下。
//!
//! 覆盖两条路径：请求显式带 bookUrl（前端主路径）；只带 chapterUrl 时靠
//! 书架记录 + 目录缓存反查出 bookUrl。API 型书源的书/章节 URL 没有公共
//! 前缀（/info 与 /content），书架 URL 前缀启发式对它们必然失效——
//! 曾经的兜底会把缓存写进 md5(chapterUrl) 孤儿目录：统计显示 0、
//! 复读不命中、按书清理清不掉。

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
        let server = Self {
            base_url,
            upstream_url,
            hits,
            temp_dir,
            client,
            token,
        };
        server.save_source().await;
        server
    }

    async fn save_source(&self) {
        let resp = self
            .client
            .post(format!("{}/reader3/saveBookSource", self.base_url))
            .bearer_auth(&self.token)
            .json(&book_source(&self.upstream_url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// 书架塞一本书：bookUrl 与章节 URL 没有公共前缀（API 型书源）。
    async fn save_book(&self) {
        let book = json!({
            "name": "测试书",
            "author": "作者",
            "bookUrl": self.book_url(),
            "origin": self.upstream_url,
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

    /// 正文缓存应当落到的文件：cache/<ns>/<md5(bookUrl)>/<md5(chapterUrl)>.txt
    fn book_cache_file(&self, chapter_url: &str) -> PathBuf {
        self.temp_dir
            .join("cache")
            .join("reader1")
            .join(md5_hex(&self.book_url()))
            .join(format!("{}.txt", md5_hex(chapter_url)))
    }

    /// 修复前的错误落点：md5(chapterUrl) 孤儿目录
    fn orphan_cache_dir(&self, chapter_url: &str) -> PathBuf {
        self.temp_dir
            .join("cache")
            .join("reader1")
            .join(md5_hex(chapter_url))
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

/// 主路径：请求显式带 bookUrl，正文直接缓存到 md5(bookUrl) 目录。
#[tokio::test]
async fn content_with_explicit_book_url_lands_in_the_book_cache_dir() {
    let server = TestServer::start().await;
    server.save_book().await;

    let body = json!({
        "chapterUrl": server.chapter_url(0),
        "bookUrl": server.book_url(),
        "bookSourceUrl": server.upstream_url,
    });
    let content = server.post("getBookContent", body.clone()).await;
    assert_eq!(content, json!("章节 100 的正文"));

    let file = server.book_cache_file(&server.chapter_url(0));
    assert!(
        file.exists(),
        "正文应缓存到 md5(bookUrl) 目录: {}",
        file.display()
    );
    assert!(
        !server.orphan_cache_dir(&server.chapter_url(0)).exists(),
        "不该产生 md5(chapterUrl) 孤儿目录"
    );

    server.post("getBookContent", body).await;
    assert_eq!(
        server.hits_for("/content"),
        1,
        "第二次应命中缓存，不再打上游"
    );
}

/// 兜底路径：只传 chapterUrl（老版本前端的请求形态）。书架 URL 前缀启发式
/// 对 /info 与 /content 必然失效，靠目录缓存反查出 bookUrl。
#[tokio::test]
async fn content_without_book_url_is_recovered_through_the_cached_toc() {
    let server = TestServer::start().await;
    server.save_book().await;

    // 先建目录缓存（阅读器进正文前必然先拉目录，这是反查的事实基础）
    let chapters = server
        .post(
            "getChapterList",
            json!({
                "bookUrl": server.book_url(),
                "tocUrl": server.toc_url(),
                "bookSourceUrl": server.upstream_url,
            }),
        )
        .await;
    let list = chapters.as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0]["url"], json!(server.chapter_url(0)));

    let body = json!({
        "chapterUrl": server.chapter_url(0),
        "bookSourceUrl": server.upstream_url,
    });
    let content = server.post("getBookContent", body.clone()).await;
    assert_eq!(content, json!("章节 100 的正文"));

    let file = server.book_cache_file(&server.chapter_url(0));
    assert!(
        file.exists(),
        "反查命中后正文应缓存到 md5(bookUrl) 目录: {}",
        file.display()
    );
    assert!(
        !server.orphan_cache_dir(&server.chapter_url(0)).exists(),
        "不该产生 md5(chapterUrl) 孤儿目录"
    );

    server.post("getBookContent", body).await;
    assert_eq!(
        server.hits_for("/content"),
        1,
        "第二次应命中缓存，不再打上游"
    );
}
