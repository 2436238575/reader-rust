//! 正文回源 singleflight 与到期回源策略的端到端行为：
//! - 同一章并发未命中只打一次上游（等待者二次检查复用缓存）；
//! - `get_content` 遇到到期缓存先返回缓存并后台回源，不阻塞调用方；
//! - 强刷（refresh 语义）仍同步打上游。

mod common;
use axum::{extract::Query, routing::get, Json, Router};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use reader_rust::model::book_source::BookSource;
use reader_rust::service::book_service::BookService;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type HitCounter = Arc<Mutex<HashMap<String, usize>>>;

/// 假上游：/catalog 给目录，/content 给正文（带 300ms 延迟，拉宽并发窗口）。
async fn start_upstream(hits: HitCounter) -> String {
    let counter = hits.clone();
    let app = Router::new()
        .route(
            "/catalog",
            get(|headers: axum::http::HeaderMap| async move {
                let host = headers
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                Json(json!({
                    "data": { "list": [
                        { "title": "第1章", "url": format!("http://{host}/content?item_id=1") },
                        { "title": "第2章", "url": format!("http://{host}/content?item_id=2") },
                    ] }
                }))
            }),
        )
        .route(
            "/content",
            get(|Query(params): Query<HashMap<String, String>>| async move {
                let item = params.get("item_id").cloned().unwrap_or_default();
                tokio::time::sleep(Duration::from_millis(300)).await;
                Json(json!({ "data": { "content": format!("章节 {item} 的正文内容") } }))
            }),
        );
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
    format!("http://{addr}")
}

struct TestServer {
    base_url: String,
    upstream_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
    service: std::sync::Arc<BookService>,
    hits: HitCounter,
}

impl TestServer {
    /// `revalidate_ttl_secs`：到期回源间隔（测试里用 1s 触发到期路径）。
    async fn start(revalidate_ttl_secs: u64) -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-flight-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let hits: HitCounter = Arc::new(Mutex::new(HashMap::new()));
        let upstream_url = start_upstream(hits.clone()).await;

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "flight-test-secret".to_string(),
            request_timeout_secs: 10,
            admin_username: "reader1".to_string(),
            admin_password: "password123".to_string(),
            rate_limit_disabled: true,
            // 假上游监听 127.0.0.1：显式放行私网出站（默认拦截）
            allow_private_network: true,
            content_revalidate_ttl_secs: revalidate_ttl_secs,
            ..AppConfig::default()
        };

        let state = build_state(cfg).await.unwrap();
        let service = state.book_service.clone();
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
        let token = resp.json::<Value>().await.unwrap()["data"]["accessToken"]
            .as_str()
            .unwrap()
            .to_string();
        Self {
            base_url,
            upstream_url,
            temp_dir,
            client,
            token,
            service,
            hits,
        }
    }

    /// 保存书源并返回结构体形态（给 service 层直调）。
    async fn save_source(&self, upstream_url: &str) -> BookSource {
        let source = json!({
            "bookSourceName": "单飞书源",
            "bookSourceUrl": upstream_url,
            "concurrentRate": "0",
            "enabled": true,
            "ruleToc": {
                "chapterList": "$.data.list[*]",
                "chapterName": "$.title",
                "chapterUrl": "$.url"
            },
            "ruleContent": { "content": "$.data.content" }
        });
        let resp = self
            .client
            .post(format!("{}/reader3/saveBookSource", self.base_url))
            .bearer_auth(&self.token)
            .json(&source)
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        assert_eq!(payload["isSuccess"], json!(true), "保存书源: {payload}");
        serde_json::from_value(source).unwrap()
    }

    /// 入架一本书，返回 bookId。
    async fn save_book(&self, upstream_url: &str) -> String {
        let resp = self
            .client
            .post(format!("{}/reader3/saveBook", self.base_url))
            .bearer_auth(&self.token)
            .json(&json!({
                "name": "单飞测试书",
                "author": "作者",
                "bookUrl": format!("{upstream_url}/catalog"),
                "tocUrl": format!("{upstream_url}/catalog"),
            }))
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        assert_eq!(payload["isSuccess"], json!(true), "入架: {payload}");
        payload["data"]["bookId"].as_str().unwrap().to_string()
    }

    fn hits_for(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

const NS: &str = "user_1";

/// 同一章并发未命中：上游只被打一次，两个请求拿到相同正文。
#[tokio::test]
async fn concurrent_content_miss_hits_upstream_once() {
    let server = TestServer::start(86400).await;
    let source = server.save_source(&server.upstream_url).await;
    let book_id = server.save_book(&server.upstream_url).await;
    let chapter = format!("{}/content?item_id=1", server.upstream_url);

    let (a, b) = tokio::join!(
        server.service.get_content(NS, &book_id, &source, &chapter),
        server.service.get_content(NS, &book_id, &source, &chapter)
    );
    let a = a.expect("首个请求应成功");
    let b = b.expect("等待者应复用结果");
    assert_eq!(a, b, "并发未命中的两个请求应拿到相同正文");
    assert_eq!(
        server.hits_for("/content"),
        1,
        "singleflight：上游只能被打一次"
    );
}

/// 并发 read_content 未命中同样只打一次上游（阅读器主路径）。
#[tokio::test]
async fn concurrent_read_content_miss_hits_upstream_once() {
    let server = TestServer::start(86400).await;
    let source = server.save_source(&server.upstream_url).await;
    let book_id = server.save_book(&server.upstream_url).await;
    let chapter = format!("{}/content?item_id=2", server.upstream_url);

    let (a, b) = tokio::join!(
        server
            .service
            .read_content(NS, &book_id, &source, &chapter, false),
        server
            .service
            .read_content(NS, &book_id, &source, &chapter, false)
    );
    let a = a.expect("首个请求应成功");
    let b = b.expect("等待者应复用结果");
    assert_eq!(a.view.content, b.view.content);
    assert_eq!(
        server.hits_for("/content"),
        1,
        "singleflight：上游只能被打一次"
    );
}

/// 到期缓存 + 非强刷：先立即返回缓存（不等上游），后台补一次回源。
#[tokio::test]
async fn stale_content_returns_cache_first_and_revalidates_in_background() {
    let server = TestServer::start(1).await;
    let source = server.save_source(&server.upstream_url).await;
    let book_id = server.save_book(&server.upstream_url).await;
    let chapter = format!("{}/content?item_id=1", server.upstream_url);

    // 首次：未命中 → 同步抓一次（~300ms，受上游延迟）
    let first = server
        .service
        .get_content(NS, &book_id, &source, &chapter)
        .await
        .expect("首次抓取应成功");
    assert_eq!(server.hits_for("/content"), 1);

    // 回源间隔 1s：等过期
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // 到期后再取：立即返回缓存（不等 300ms 的上游），后台再回源
    let began = std::time::Instant::now();
    let cached = server
        .service
        .get_content(NS, &book_id, &source, &chapter)
        .await
        .expect("到期读取应回退缓存");
    assert_eq!(cached, first, "到期读取返回的应是缓存正文");
    assert!(
        began.elapsed() < Duration::from_millis(250),
        "到期读取不应同步等上游（实际 {:?}）",
        began.elapsed()
    );

    // 后台回源最终完成：上游被再打一次（总共 2 次，且只有 2 次）
    let mut background_hits = 0usize;
    for _ in 0..40 {
        background_hits = server.hits_for("/content");
        if background_hits >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(background_hits, 2, "后台回源应恰好再抓一次");
}

/// 强刷路径（refresh 语义）：即使缓存新鲜也必须再打一次上游。
#[tokio::test]
async fn forced_refresh_still_hits_upstream_when_cache_is_fresh() {
    let server = TestServer::start(86400).await;
    let source = server.save_source(&server.upstream_url).await;
    let book_id = server.save_book(&server.upstream_url).await;
    let chapter = format!("{}/content?item_id=1", server.upstream_url);

    let _ = server
        .service
        .get_content(NS, &book_id, &source, &chapter)
        .await
        .unwrap();
    assert_eq!(server.hits_for("/content"), 1);

    // 缓存完全新鲜，但强刷不做二次检查：必须真的再打上游
    let _ = server
        .service
        .refresh_chapter_content(NS, &book_id, &source, &chapter)
        .await
        .expect("强刷应成功");
    assert_eq!(server.hits_for("/content"), 2, "强刷必须再打一次上游");
}
