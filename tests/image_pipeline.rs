//! 图片管道的端到端行为：书源地址收口成 id、HEIC 转 JPEG、签名失效后回源自愈。
//!
//! 上游用假服务顶替：`/cover.heic` 只认当前签名，旧签名一律 403——
//! 番茄图床就是这个行为（签名随宿主会话轮换），正好用来验证自愈。

mod common;
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
        // 慢速小图（300ms）：拉宽并发首取的竞态窗口
        .route(
            "/slow.jpg",
            get(|| async {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let mut resp = Response::new(axum::body::Body::from(TINY_HEIC.to_vec()));
                resp.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("image/jpeg"),
                );
                resp
            }),
        )
        // 慢速 HEIC（300ms）：并发首取时首个请求会转码成 JPEG 并改写记录，
        // 等待者二次检查命中后必须重读记录，否则用过期类型配转码字节
        .route(
            "/slow.heic",
            get(|| async {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let mut resp = Response::new(axum::body::Body::from(TINY_HEIC.to_vec()));
                resp.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("image/heic"),
                );
                resp
            }),
        )
        // 128x128 PNG：按宽度派生 + webp 协商的解码源
        .route(
            "/pic.png",
            get(|| async {
                let mut resp = Response::new(axum::body::Body::from(make_png(128, 128)));
                resp.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("image/png"),
                );
                resp
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
    book_source_id: std::cell::Cell<i64>,
}

impl TestServer {
    async fn start() -> Self {
        let (upstream_url, _hits) = start_upstream().await;
        common::clean_stale_temp_dirs();
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
        let server = Self {
            base_url,
            upstream_url,
            temp_dir,
            client,
            token,
            book_source_id: std::cell::Cell::new(0),
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
        let payload: Value = resp.json().await.unwrap();
        self.book_source_id
            .set(payload["data"]["bookSourceId"].as_i64().unwrap());
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
            "bookSourceId": self.book_source_id.get(),
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
            // 会话在 HttpOnly Cookie 里：图片端点不再接受查询串主令牌
            .header("Cookie", format!("reader_access={}", self.token))
            .send()
            .await
            .unwrap()
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

/// 并发首取同一张图：按 id 单飞，上游只被打一次，两个请求拿到相同字节。
#[tokio::test]
async fn concurrent_first_load_of_same_image_hits_upstream_once() {
    let server = TestServer::start().await;
    // 入架一本封面指向慢速图（300ms）的书：书架改写只登记映射、不抓图，
    // 随后两个取图请求都是未命中
    let book = json!({
        "name": "并发取图书",
        "author": "作者",
        "bookUrl": server.book_url(),
        "bookSourceId": server.book_source_id.get(),
        "coverUrl": format!("{}/slow.jpg?sig=1", server.upstream_url),
    });
    let resp = server
        .client
        .post(format!("{}/reader3/saveBook", server.base_url))
        .bearer_auth(&server.token)
        .json(&book)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let route = server.shelf_cover_route().await;
    assert!(route.starts_with("/reader3/image/"), "实际 {route}");

    let (a, b) = tokio::join!(server.get_image(&route), server.get_image(&route));
    assert_eq!(a.status(), StatusCode::OK);
    assert_eq!(b.status(), StatusCode::OK);
    let (bytes_a, bytes_b) = (a.bytes().await.unwrap(), b.bytes().await.unwrap());
    assert_eq!(bytes_a, bytes_b, "两个请求应拿到相同字节");

    let hits: HashMap<String, usize> = {
        let resp = server
            .client
            .get(format!("{}/__hits", server.upstream_url))
            .send()
            .await
            .unwrap();
        resp.json().await.unwrap()
    };
    assert_eq!(
        hits.get("/slow.jpg").copied().unwrap_or(0),
        1,
        "单飞：并发首取同一张图上游只能被打一次: {hits:?}"
    );
}

/// 并发首取一张需要转码的 HEIC：首个请求转成 JPEG 并更新记录里的类型，
/// 等待者拿到锁后二次检查命中 body 文件时必须重读记录——
/// 否则会拿登记时的 `application/octet-stream` 配转码后的 JPEG 字节。
#[tokio::test]
async fn concurrent_first_load_of_heic_returns_fresh_content_type() {
    let server = TestServer::start().await;
    let book = json!({
        "name": "并发HEIC书",
        "author": "作者",
        "bookUrl": server.book_url(),
        "bookSourceId": server.book_source_id.get(),
        "coverUrl": format!("{}/slow.heic?sig=1", server.upstream_url),
    });
    let resp = server
        .client
        .post(format!("{}/reader3/saveBook", server.base_url))
        .bearer_auth(&server.token)
        .json(&book)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let route = server.shelf_cover_route().await;
    assert!(route.starts_with("/reader3/image/"), "实际 {route}");

    let (a, b) = tokio::join!(server.get_image(&route), server.get_image(&route));
    assert_eq!(a.status(), StatusCode::OK);
    assert_eq!(b.status(), StatusCode::OK);
    for (label, value) in [("a", &a), ("b", &b)] {
        assert_eq!(
            value
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("image/jpeg"),
            "等待者也要用转码后的最新类型（请求 {label}）"
        );
    }
    let (bytes_a, bytes_b) = (a.bytes().await.unwrap(), b.bytes().await.unwrap());
    assert_eq!(&bytes_a[..2], &[0xFF, 0xD8], "响应体应当是 JPEG");
    assert_eq!(bytes_a, bytes_b, "两个请求应拿到相同字节");

    let hits: HashMap<String, usize> = {
        let resp = server
            .client
            .get(format!("{}/__hits", server.upstream_url))
            .send()
            .await
            .unwrap();
        resp.json().await.unwrap()
    };
    assert_eq!(
        hits.get("/slow.heic").copied().unwrap_or(0),
        1,
        "单飞：并发首取 HEIC 上游只能被打一次: {hits:?}"
    );
}

/// 生成一张着色 PNG（load_variant 端到端用例的解码源）。
fn make_png(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([
            ((x * 255) / width.max(1)) as u8,
            (y % 256) as u8,
            0x80,
            0xFF,
        ])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// handler 级：`?w=` 按宽度派生、Accept 协商 webp、响应带 `Vary: Accept`；
/// 兼容入口 `/reader3/cover` 同样支持 `w`。
#[tokio::test]
async fn image_endpoint_negotiates_webp_and_scales() {
    let server = TestServer::start().await;
    let png_url = format!("{}/pic.png", server.upstream_url);
    let id = reader_rust::service::image_service::ImageService::image_key(&png_url);

    // 0) 兼容入口先登记这张图（不带 w 也不带 webp 的旧调用方式：原样 PNG，
    //    行为与改动前一致），顺便验证 id 推导正确
    let resp = server
        .client
        .get(format!("{}/reader3/cover", server.base_url))
        .bearer_auth(&server.token)
        .query(&[("path", png_url.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/png"),
        "无 w 无 webp 时原图直出"
    );
    let bytes = resp.bytes().await.unwrap();
    let decoded = image::load_from_memory(&bytes).expect("应当是原样 PNG");
    assert_eq!(decoded.width(), 128, "无 w 时不缩放");

    // 1) w=64 + Accept 含 image/webp：webp 输出 + Vary: Accept
    let resp = server
        .client
        .get(format!("{}/reader3/image/{id}", server.base_url))
        .query(&[("w", "64")])
        .header("Cookie", format!("reader_access={}", server.token))
        .header(
            "Accept",
            "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/webp"),
        "Accept 含 image/webp 时应输出 webp"
    );
    assert_eq!(
        resp.headers()
            .get(header::VARY)
            .and_then(|v| v.to_str().ok()),
        Some("Accept"),
        "内容随 Accept 变化，必须带 Vary: Accept"
    );
    let bytes = resp.bytes().await.unwrap();
    let decoded = webp::Decoder::new(&bytes)
        .decode()
        .expect("应当是可解码的 webp");
    assert_eq!(decoded.width(), 64, "应当缩到 64 宽");

    // 2) 同一 id 不带 webp 的 Accept：JPEG + Vary: Accept（同一 URL 两种表示）
    let resp = server
        .client
        .get(format!("{}/reader3/image/{id}", server.base_url))
        .query(&[("w", "64")])
        .header("Cookie", format!("reader_access={}", server.token))
        .header("Accept", "image/jpeg,image/png")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/jpeg"),
        "Accept 不含 webp 时应输出 JPEG"
    );
    assert_eq!(
        resp.headers()
            .get(header::VARY)
            .and_then(|v| v.to_str().ok()),
        Some("Accept")
    );
    let bytes = resp.bytes().await.unwrap();
    let decoded = image::load_from_memory(&bytes).expect("应当是可解码的 JPEG");
    assert_eq!(decoded.width(), 64);

    // 3) 兼容入口 /reader3/cover 也支持 w
    let resp = server
        .client
        .get(format!("{}/reader3/cover", server.base_url))
        .bearer_auth(&server.token)
        .query(&[("path", png_url.as_str()), ("w", "64")])
        .header("Accept", "image/webp")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("image/webp"),
        "兼容入口也应按 Accept 输出 webp"
    );
    let bytes = resp.bytes().await.unwrap();
    let decoded = webp::Decoder::new(&bytes)
        .decode()
        .expect("应当是可解码的 webp");
    assert_eq!(decoded.width(), 64, "兼容入口也应按 w 缩放");
}
