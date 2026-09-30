//! 章节配图的端到端行为：配图规则（独立接口）与正文 HTML 内嵌图片两条路。
//!
//! 上游用一个假的「番茄式」服务顶替（真实 FQWeb 需要一台装着番茄小说的手机），
//! 响应结构与真实接口一致，因此书源规则、URL 求值、解析走的都是生产路径。

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
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

const BOOK_ID: &str = "7406592861791063064";
/// 1x1 的最小 JPEG。
const TINY_JPEG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08, 0x07, 0x07,
    0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B, 0x0C, 0x19, 0x12, 0x13, 0x0F,
    0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20, 0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C,
    0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D,
    0x38, 0x32, 0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01,
    0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xFF, 0xC4, 0x00, 0x14, 0x10, 0x01,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x37, 0xFF, 0xD9,
];

const ITEM_ID: &str = "7406592932351836696";

type HitCounter = Arc<Mutex<HashMap<String, usize>>>;

/// 假上游：正文 + 配图接口。
async fn start_upstream() -> (String, HitCounter) {
    let hits: HitCounter = Arc::new(Mutex::new(HashMap::new()));

    let content = json!({
        "data": { "data": {
            "content": "第一段正文\n第二段正文\n配图（画师：奈月Oo）",
            "novel_data": { "book_id": BOOK_ID, "item_id": ITEM_ID }
        }}
    });

    // 有配图的章节：位置指向「配图（画师：奈月Oo）」那一行（下标 2）
    let images = json!({
        "data": {
            "item_id": ITEM_ID,
            "has_image": true,
            "images": [{
                "url": "/chapter-image.jpeg?x-expires=1884944022&x-signature=abc%3D",
                "width": 1400,
                "height": 933,
                "para_index": 2,
                "caption": "配图（画师：奈月Oo）"
            }]
        }
    });

    let empty_images = json!({
        "data": { "item_id": "other", "has_image": false, "images": [] }
    });

    let app = Router::new()
        .route(
            "/content",
            get({
                let content = content.clone();
                move |Query(params): Query<HashMap<String, String>>| {
                    let mut body = content.clone();
                    // 换一个 item_id 就换成「没有配图」的章节
                    if params.get("item_id").map(String::as_str) != Some(ITEM_ID) {
                        body["data"]["data"]["novel_data"]["item_id"] = json!("other");
                    }
                    async move { Json(body) }
                }
            }),
        )
        .route(
            "/content/image",
            get({
                let images = images.clone();
                let empty_images = empty_images.clone();
                move |Query(params): Query<HashMap<String, String>>| {
                    let body = if params.get("item_id").map(String::as_str) == Some(ITEM_ID) {
                        images.clone()
                    } else {
                        empty_images.clone()
                    };
                    async move { Json(body) }
                }
            }),
        )
        .route(
            "/chapter-image.jpeg",
            get(|| async {
                let mut resp = Response::new(axum::body::Body::from(TINY_JPEG.to_vec()));
                resp.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("image/jpeg"),
                );
                resp
            }),
        )
        .route(
            "/rich",
            get(|| async {
                // 正文 HTML 内嵌图片的书源：正文规则直接吐出 HTML
                Json(json!({
                    "data": { "xhtml": "<p>第一段</p>\
                        <div data-fanqie-type=\"image\"><p class=\"picture\">\
                        <img src=\"https://img.example/inline.jpeg\" img-width=\"1400\" img-height=\"933\"/>\
                        </p><p class=\"pictureDesc\">配图（画师：奈月Oo）</p></div>" }
                }))
            }),
        )
        .route(
            "/__hits",
            get(|State(hits): State<HitCounter>| async move {
                let snapshot: HashMap<String, usize> = hits.lock().unwrap().clone();
                Json(serde_json::to_value(snapshot).unwrap())
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

/// 书源：正文走 `/content`，配图走独立的 `/content/image`。
fn image_source(upstream_url: &str) -> Value {
    json!({
        "bookSourceName": "假番茄",
        "bookSourceUrl": upstream_url,
        "concurrentRate": "0",
        "enabled": true,
        "ruleContent": { "content": "$.data.data.content" },
        "ruleContentImage": {
            "imageUrl": "content/image?item_id={{$.data.data.novel_data.item_id}}",
            "listRule": "$.data.images[*]"
        }
    })
}

/// 书源：正文规则直接返回 HTML（图片内嵌在正文里），没有配图规则。
fn rich_source(upstream_url: &str) -> Value {
    json!({
        "bookSourceName": "假富文本",
        "bookSourceUrl": upstream_url,
        "concurrentRate": "0",
        "enabled": true,
        "ruleContent": { "content": "$.data.xhtml" }
    })
}

impl TestServer {
    async fn start(source: Value) -> Self {
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
            jwt_secret: "image-test-secret".to_string(),
            request_timeout_secs: 10,
            // bootstrap 用预设凭据建号，测试直接登录
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
        let server = Self {
            base_url,
            upstream_url,
            temp_dir,
            client,
            token,
        };
        let mut source = source;
        source["bookSourceUrl"] = json!(server.upstream_url);
        server.save_source(&source).await;
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
        assert_eq!(payload["isSuccess"], json!(true), "{path} 应成功");
        payload["data"].clone()
    }

    fn chapter_params(&self, chapter_url: &str) -> Value {
        json!({
            "bookUrl": format!("{}/info?book_id={BOOK_ID}", self.upstream_url),
            "chapterUrl": chapter_url,
            "bookSourceUrl": self.upstream_url,
        })
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
        .json(&json!({ "username": username, "password": "password123" }))
        .send()
        .await
        .unwrap();
    let payload: Value = resp.json().await.unwrap();
    assert_eq!(payload["isSuccess"], json!(true), "登录应成功: {payload}");
    payload["data"]["accessToken"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn chapter_images_come_from_the_source_rule() {
    let server = TestServer::start(image_source("")).await;
    let chapter_url = format!("{}/content?item_id={ITEM_ID}", server.upstream_url);

    let data = server
        .post("getChapterImages", server.chapter_params(&chapter_url))
        .await;

    assert_eq!(data["enabled"], json!(true));
    let images = data["images"].as_array().unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["caption"], json!("配图（画师：奈月Oo）"));
    assert_eq!(images[0]["paraIndex"], json!(2));
    assert_eq!(images[0]["width"], json!(1400));
    assert_eq!(images[0]["height"], json!(933));
    // 图片地址收口成本站取图接口：不再把带签名的书源地址交给前端
    let route = images[0]["url"].as_str().unwrap();
    assert!(route.starts_with("/reader3/image/"), "实际 {route}");
    assert!(
        !route.contains("x-signature"),
        "不该把签名地址交给前端：{route}"
    );

    // 真取一次：能拿到上游那张图
    let resp = server
        .client
        .get(format!("{}{route}", server.base_url))
        .query(&[("accessToken", server.token.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "配图应当取得到：{route}");
    let bytes = resp.bytes().await.unwrap();
    assert_eq!(&bytes[..2], &[0xFF, 0xD8], "应当是 JPEG");
}

#[tokio::test]
async fn chapter_without_images_is_not_an_error() {
    let server = TestServer::start(image_source("")).await;
    // item_id 不是常量时假上游返回空列表
    let chapter_url = format!("{}/content?item_id=another", server.upstream_url);

    let data = server
        .post("getChapterImages", server.chapter_params(&chapter_url))
        .await;

    assert_eq!(
        data["enabled"],
        json!(true),
        "书源有配图规则就应当是 enabled"
    );
    assert_eq!(data["images"], json!([]));
}

#[tokio::test]
async fn source_without_image_rule_reports_disabled() {
    let server = TestServer::start(rich_source("")).await;
    let chapter_url = format!("{}/rich", server.upstream_url);

    let data = server
        .post("getChapterImages", server.chapter_params(&chapter_url))
        .await;

    assert_eq!(data["enabled"], json!(false));
    assert_eq!(data["images"], json!([]));
}

#[tokio::test]
async fn html_content_keeps_inline_images() {
    let server = TestServer::start(rich_source("")).await;
    let chapter_url = format!("{}/rich", server.upstream_url);

    let content = server
        .post("getBookContent", server.chapter_params(&chapter_url))
        .await;
    let content = content.as_str().unwrap();

    // 正文 HTML 里的图片必须原样保留，前端才有得渲染
    assert!(content.contains("<img"), "正文应保留 <img>: {content}");
    assert!(
        content.contains("src=\"https://img.example/inline.jpeg\""),
        "图片地址应当完整: {content}"
    );
    assert!(content.contains("配图（画师：奈月Oo）"));
}
