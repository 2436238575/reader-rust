//! 正文缓存的落盘位置：getBookContent 必须把正文缓存到 `cache/<ns>/<bookId>/` 目录下。
//!
//! bookId 是唯一身份参数：曾经「只带 chapterUrl 靠书架/目录缓存反推 bookUrl」的
//! 兜底链路已删除（API 型书源的书/章节 URL 没有公共前缀，反推失败会把缓存写进
//! md5(chapterUrl) 孤儿目录）。现在请求缺 bookId 直接 400，孤儿目录无从产生。

mod common;
use axum::{extract::Query, http::StatusCode, routing::get, Json, Router};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
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
        // 无限翻页源：每页都给不同的下一页地址（病态书源场景）
        .route(
            "/content_paged",
            get(|headers: axum::http::HeaderMap| async move {
                let host = headers
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                Json(json!({
                    "data": { "data": {
                        "content": "一页正文",
                        "next": format!("http://{host}/content_paged?t={}", uuid::Uuid::new_v4()),
                    } }
                }))
            }),
        )
        // 无限翻页 + 超大单页（3MB）：逼出累计字节上限（病态书源场景）
        .route(
            "/content_paged_big",
            get(|headers: axum::http::HeaderMap| async move {
                let host = headers
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                // 3MB 文本页：3 页就超过 8MB 的累计上限
                let big_page = "巨".repeat(1024 * 1024);
                Json(json!({
                    "data": { "data": {
                        "content": big_page,
                        "next": format!("http://{host}/content_paged_big?t={}", uuid::Uuid::new_v4()),
                    } }
                }))
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

        common::clean_stale_temp_dirs();
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

    fn hits_for(&self, path: &str) -> usize {
        *self.hits.lock().unwrap().get(path).unwrap_or(&0)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
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
    assert_eq!(content["content"], json!("章节 100 的正文"));

    let timeline = server
        .post(
            "getChapterTimeline",
            json!({ "bookId": server.book_id, "chapterUrl": server.chapter_url(0) }),
        )
        .await;
    let items = timeline["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "每次成功抓取应落一个上游版本");
    assert_eq!(items[0]["kind"], json!("upstream"));

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

    // 正文已经进数据库：缓存目录里不该再出现按书/章节命名的正文目录
    let orphan = server
        .temp_dir
        .join("cache")
        .join("user_1")
        .join(&server.book_id);
    assert!(
        !orphan.exists(),
        "正文不应再落文件缓存: {}",
        orphan.display()
    );
}

#[tokio::test]
async fn content_pagination_stops_at_page_cap() {
    let server = TestServer::start().await;

    // 挂一个带 nextContentUrl 规则的书源（翻页地址每次都不同 → 环检测挡不住）
    let source = json!({
        "bookSourceName": "无限翻页源",
        "bookSourceUrl": server.upstream_url,
        "enabled": true,
        "ruleContent": {
            "content": "$.data.data.content",
            "nextContentUrl": "$.data.data.next"
        },
        "ruleToc": {
            "chapterList": "$.data.list[*]",
            "chapterName": "$.title",
            "chapterUrl": "$.url"
        }
    });
    let resp = server
        .client
        .post(format!("{}/reader3/saveBookSource", server.base_url))
        .bearer_auth(&server.token)
        .json(&source)
        .send()
        .await
        .unwrap();
    let source_id: i64 = resp.json::<Value>().await.unwrap()["data"]["bookSourceId"]
        .as_i64()
        .unwrap();

    // 入架一本书（目录随便指，正文直接打无限翻页端点）
    let book = json!({
        "name": "无限翻页书",
        "author": "作者",
        "bookUrl": format!("{}/info?book_id=paged", server.upstream_url),
        "bookSourceId": source_id,
        "tocUrl": format!("{}/catalog?book_id=paged", server.upstream_url),
    });
    let resp = server
        .client
        .post(format!("{}/reader3/saveBook", server.base_url))
        .bearer_auth(&server.token)
        .json(&book)
        .send()
        .await
        .unwrap();
    let book_id = resp.json::<Value>().await.unwrap()["data"]["bookId"]
        .as_str()
        .unwrap()
        .to_string();

    let content = server
        .post(
            "getBookContent",
            json!({
                "bookId": book_id,
                "chapterUrl": format!("{}/content_paged", server.upstream_url),
            }),
        )
        .await;

    // 上限 50 页截断：上游被恰好打 50 次，正文恰好 50 页拼起来
    assert_eq!(server.hits_for("/content_paged"), 50, "翻页必须被上限截断");
    let pages = content["content"]
        .as_str()
        .unwrap()
        .matches("一页正文")
        .count();
    assert_eq!(pages, 50, "正文应恰好拼 50 页");
}

#[tokio::test]
async fn content_pagination_stops_at_accumulated_byte_cap() {
    let server = TestServer::start().await;

    // 同一无限翻页形态，但单页 3MB：3 页就超过 8MB 累计上限
    let source = json!({
        "bookSourceName": "无限翻页大正文源",
        "bookSourceUrl": server.upstream_url,
        "enabled": true,
        "ruleContent": {
            "content": "$.data.data.content",
            "nextContentUrl": "$.data.data.next"
        },
        "ruleToc": {
            "chapterList": "$.data.list[*]",
            "chapterName": "$.title",
            "chapterUrl": "$.url"
        }
    });
    let resp = server
        .client
        .post(format!("{}/reader3/saveBookSource", server.base_url))
        .bearer_auth(&server.token)
        .json(&source)
        .send()
        .await
        .unwrap();
    let source_id: i64 = resp.json::<Value>().await.unwrap()["data"]["bookSourceId"]
        .as_i64()
        .unwrap();

    let book = json!({
        "name": "无限翻页大正文书",
        "author": "作者",
        "bookUrl": format!("{}/info?book_id=paged-big", server.upstream_url),
        "bookSourceId": source_id,
        "tocUrl": format!("{}/catalog?book_id=paged-big", server.upstream_url),
    });
    let resp = server
        .client
        .post(format!("{}/reader3/saveBook", server.base_url))
        .bearer_auth(&server.token)
        .json(&book)
        .send()
        .await
        .unwrap();
    let book_id = resp.json::<Value>().await.unwrap()["data"]["bookId"]
        .as_str()
        .unwrap()
        .to_string();

    let content = server
        .post(
            "getBookContent",
            json!({
                "bookId": book_id,
                "chapterUrl": format!("{}/content_paged_big", server.upstream_url),
            }),
        )
        .await;

    // 累计 8MB 上限先于 50 页上限触发：第 3 页拼完后 9MB ≥ 8MB，立即停止
    assert_eq!(
        server.hits_for("/content_paged_big"),
        3,
        "累计字节上限必须在页数上限之前截断翻页"
    );
    // 保留的是完整页面（3 × 3MB + 分隔符），不做半页静默截断
    let len = content["content"].as_str().unwrap().len();
    assert_eq!(len, 3 * 3 * 1024 * 1024 + 2, "恰好 3 个完整 3MB 页面");
}
