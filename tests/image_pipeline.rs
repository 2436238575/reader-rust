//! 图片管道的端到端行为：书源地址收口成 id、HEIC 转 JPEG、签名失效后回源自愈。
//!
//! 上游用假服务顶替：`/cover.heic` 只认当前签名，旧签名一律 403——
//! 番茄图床就是这个行为（签名随宿主会话轮换），正好用来验证自愈。

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 16x16 的 HEIC 测试夹具（纯色，几百字节）。
const TINY_HEIC: &[u8] = include_bytes!("fixtures/tiny.heic");
const BOOK_ID: &str = "1";
/// 只有这一个签名有效；其余（模拟过期签名）一律 403。
const FRESH_SIG: &str = "fresh";

type Counter = Arc<Mutex<HashMap<String, usize>>>;

async fn start_upstream() -> (String, Counter) {
    let hits: Counter = Arc::new(Mutex::new(HashMap::new()));

    // 每次取详情都换一个新签名：模拟番茄「地址很快过期」的行为
    let app = Router::new()
        .route(
            "/info",
            get({
                let hits = hits.clone();
                move || {
                    let hits = hits.clone();
                    async move {
                        let mut map = hits.lock().unwrap();
                        let n = map.entry("info".to_string()).or_insert(0);
                        *n += 1;
                        let sig = if *n > 1 { FRESH_SIG } else { "stale" };
                        Json(json!({
                            "data": { "data": {
                                "book_id": BOOK_ID,
                                "book_name": "假书",
                                "thumb_url": format!("/cover.heic?sig={sig}&x-expires=1&x-signature=abc"),
                            }}
                        }))
                    }
                }
            }),
        )
        .route(
            "/cover.heic",
            get(|Query(params): Query<HashMap<String, String>>| async move {
                if params.get("sig").map(String::as_str) == Some(FRESH_SIG) {
                    let mut resp = Response::new(axum::body::Body::from(TINY_HEIC.to_vec()));
                    resp.headers_mut().insert(
                        header::CONTENT_TYPE,
                        header::HeaderValue::from_static("image/heic"),
                    );
                    resp
                } else {
                    // 旧签名：上游 403
                    StatusCode::FORBIDDEN.into_response()
                }
            }),
        )
        .route(
            "/plain.jpg",
            get(|| async {
                // 一张最小的 JPEG（1x1），验证非 HEIC 原样透传
                const JPEG: &[u8] = &[
                    0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05,
                    0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B,
                    0x0B, 0x0C, 0x19, 0x12, 0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A,
                    0x1C, 0x1C, 0x20, 0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28,
                    0x37, 0x29, 0x2C, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38,
                    0x32, 0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01,
                    0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00, 0x14, 0x00, 0x01, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x03, 0xFF, 0xC4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA,
                    0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x37, 0xFF, 0xD9,
                ];
                let mut resp = Response::new(axum::body::Body::from(JPEG.to_vec()));
                resp.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("image/jpeg"),
                );
                resp
            }),
        )
        .route(
            "/__hits",
            get(|State(hits): State<Counter>| async move {
                Json(serde_json::to_value(hits.lock().unwrap().clone()).unwrap())
            }),
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
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

impl TestServer {
    async fn start() -> Self {
        let (upstream_url, _hits) = start_upstream().await;
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-image-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "image-pipeline-secret".to_string(),
            request_timeout_secs: 10,
            rate_limit_disabled: true,
            admin_username: "reader1".to_string(),
            admin_password: "password123".to_string(),
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
        let server = Self {
            base_url,
            upstream_url,
            temp_dir,
            client,
            token,
        };
        server.save_source().await;
        server
    }

    async fn save_source(&self) {
        let source = json!({
            "bookSourceName": "假图源",
            "bookSourceUrl": self.upstream_url,
            "concurrentRate": "0",
            "enabled": true,
            "ruleBookInfo": {
                "name": "$.data.data.book_name",
                "coverUrl": "$.data.data.thumb_url",
            },
            "ruleContent": { "content": "$.data.data.content" },
        });
        let resp = self
            .client
            .post(format!("{}/reader3/saveBookSource", self.base_url))
            .bearer_auth(&self.token)
            .json(&source)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    fn book_url(&self) -> String {
        format!("{}/info?book_id={BOOK_ID}", self.upstream_url)
    }

    /// 存一本封面地址是「过期签名」的书：请求封面时上游会 403。
    async fn save_book_with_stale_cover(&self) {
        let book = json!({
            "name": "假书",
            "author": "作者",
            "bookUrl": self.book_url(),
            "origin": self.upstream_url,
            "coverUrl": format!("{}/cover.heic?sig=stale&x-expires=1&x-signature=old", self.upstream_url),
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

    async fn shelf_cover_route(&self) -> String {
        let resp = self
            .client
            .get(format!("{}/reader3/getBookshelf", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        payload["data"][0]["coverUrl"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    async fn get_image(&self, route: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{}", self.base_url, route))
            .query(&[("accessToken", self.token.as_str())])
            .send()
            .await
            .unwrap()
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
async fn shelf_cover_is_rewritten_to_image_route() {
    let server = TestServer::start().await;
    server.save_book_with_stale_cover().await;

    let route = server.shelf_cover_route().await;
    assert!(
        route.starts_with("/reader3/image/"),
        "书架里的封面应当是本站取图地址，实际 {route}"
    );
    // 前端拿到的地址里不该再有书源地址（更不该有过期签名）
    assert!(!route.contains("cover.heic"));
    assert!(!route.contains("x-signature"));
}

#[tokio::test]
async fn heic_cover_is_served_as_jpeg_and_heals_expired_signature() {
    let server = TestServer::start().await;
    server.save_book_with_stale_cover().await;
    let route = server.shelf_cover_route().await;

    // 登记时留下的地址签名已失效：第一次抓取会 403，管道应当回源刷新后重试
    let resp = server.get_image(&route).await;
    assert_eq!(resp.status(), StatusCode::OK, "自愈后应当拿到图");
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/jpeg"),
        "HEIC 应当转成 JPEG"
    );
    let bytes = resp.bytes().await.unwrap();
    assert_eq!(&bytes[..2], &[0xFF, 0xD8], "响应体应当是 JPEG");
    assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9]);

    // 回源刷新确实发生过：详情被重新拉了一次
    let hits: HashMap<String, usize> = {
        let resp = server
            .client
            .get(format!("{}/__hits", server.upstream_url))
            .send()
            .await
            .unwrap();
        resp.json().await.unwrap()
    };
    assert!(
        hits.get("info").copied().unwrap_or(0) >= 2,
        "应当回源重新求值过封面地址: {hits:?}"
    );

    // 第二次取图直接命中本地缓存，不再打上游
    let before = hits.get("/cover.heic").copied().unwrap_or(0);
    let resp = server.get_image(&route).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let hits_after: HashMap<String, usize> = {
        let resp = server
            .client
            .get(format!("{}/__hits", server.upstream_url))
            .send()
            .await
            .unwrap();
        resp.json().await.unwrap()
    };
    assert_eq!(
        hits_after.get("/cover.heic").copied().unwrap_or(0),
        before,
        "缓存命中后不该再抓上游"
    );
}

#[tokio::test]
async fn non_heic_images_pass_through_and_unknown_id_is_404() {
    let server = TestServer::start().await;

    // 直接用兼容入口登记一张普通 JPEG
    let resp = server
        .client
        .get(format!("{}/reader3/cover", server.base_url))
        .bearer_auth(&server.token)
        .query(&[("path", format!("{}/plain.jpg", server.upstream_url))])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/jpeg")
    );

    // 没登记过的 id：404，而不是把上游错误暴露出去
    let resp = server.get_image("/reader3/image/deadbeefdeadbeef").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
