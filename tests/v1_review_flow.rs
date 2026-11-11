//! `/v1/books/{bookId}/chapters/{index}/comments|images`：章评/段评/配图读链路
//! 走 v1 身份（`(bookId, index)`，客户端不携带上游地址）与配图变体收口。
//!
//! 假上游与书源规则沿用 `review_flow.rs` / `chapter_image_flow.rs` 的「番茄式」
//! 夹具，评论解析、7 天缓存与配图挑选走的都是生产路径。

mod common;
use axum::{
    extract::Query,
    http::StatusCode,
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

const BOOK_ID: &str = "7143038691944959011";
const ITEM_ID: &str = "7173615518858150414";
/// 1x1 的最小 JPEG：验证配图能真的取回来。
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

const ITEM_VERSION: &str = "52754e01aa26a8dbd8c15cbcf7f4f64b_1_v5";

/// 假上游：评论链路 + 章节配图链路需要的几个接口。
async fn start_upstream() -> String {
    let content = json!({
        "data": { "data": {
            "content": "第一段正文\n第二段正文\n第三段正文",
            "novel_data": {
                "book_id": BOOK_ID,
                "item_id": ITEM_ID,
                "version": ITEM_VERSION,
            }
        }}
    });

    let chapter_comments = json!({
        "data": { "data": {
            "comment_cnt": 67,
            "has_more": true,
            "comment": [{
                "comment_id": "c1",
                "text": "第一条章评",
                "create_timestamp": 1717597455,
                "digg_count": 68,
                "reply_count": 1,
                "has_author_digg": true,
                "user_info": { "user_name": "读者甲", "user_avatar": "https://img.example/a.jpg", "is_author": true },
                "reply_list": [
                    { "text": "同感", "user_info": { "user_name": "读者乙", "is_author": true } }
                ]
            }]
        }}
    });

    let para_index = json!({
        "data": { "data": { "idea_data": {
            "0": { "idea_count": 140, "author_commented": 1 },
            "2": { "idea_count": 30 },
            "-1": { "idea_count": 9 }
        }}}
    });

    let para_comments = json!({
        "data": { "data": {
            "count": 192,
            "has_more": false,
            "comments": [{
                "comment_id": "p1",
                "text": "这一段写得真好",
                "create_timestamp": 1738911978,
                "digg_count": 216,
                "reply_count": 1,
                // 同一张图的两个格式变体：HEIC 取不到（模拟上游只认某一种），
                // JPEG 可用——验证后端挑的是能渲染的那张
                "image_url": ["/a.heic?sign=1", "/a.jpeg?sign=2"],
                "user_info": { "user_name": "读者丙" },
                "reply_list": [
                    { "text": "同感", "user_info": { "user_name": "读者丁" } }
                ]
            }]
        }}
    });

    let chapter_images = json!({
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

    let app = Router::new()
        .route(
            "/content",
            get(move || async move { Json(content.clone()) }),
        )
        .route(
            "/comment/item",
            get(|Query(params): Query<HashMap<String, String>>| async move {
                let mut body = chapter_comments.clone();
                if params.get("sort").map(String::as_str) == Some("time") {
                    body["data"]["data"]["comment"][0]["text"] = json!("最新序第一条");
                }
                Json(body)
            }),
        )
        .route(
            "/a.heic",
            get(|| async { StatusCode::FORBIDDEN.into_response() }),
        )
        .route(
            "/a.jpeg",
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
            "/comment/para/list",
            get(move || async move { Json(para_index.clone()) }),
        )
        .route(
            "/comment/para",
            get(|Query(params): Query<HashMap<String, String>>| async move {
                let mut body = para_comments.clone();
                if params.get("sort").map(String::as_str) == Some("time_desc") {
                    body["data"]["data"]["comments"][0]["text"] = json!("最新序段评");
                }
                Json(body)
            }),
        )
        .route(
            "/content/image",
            get(move || async move { Json(chapter_images.clone()) }),
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
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
    book_id: String,
}

fn book_source(upstream_url: &str) -> Value {
    json!({
        "bookSourceName": "假番茄",
        "bookSourceUrl": upstream_url,
        "concurrentRate": "0",
        "enabled": true,
        "ruleContent": { "content": "$.data.data.content" },
        "ruleContentImage": {
            "imageUrl": "content/image?item_id={{$.data.data.novel_data.item_id}}",
            "listRule": "$.data.images[*]"
        },
        "ruleReview": {
            "reviewUrl": "comment/item?item_id={{$.data.data.novel_data.item_id}}&book_id={{$.data.data.novel_data.book_id}}&page={{page}}&count={{count}}&sort={{sort === 'hot' ? 'smart_hot' : 'time'}}",
            "listRule": "$.data.data.comment[*]",
            "totalRule": "$.data.data.comment_cnt",
            "hasMoreRule": "$.data.data.has_more",
            "idRule": "$.comment_id",
            "nameRule": "$.user_info.user_name",
            "avatarRule": "$.user_info.user_avatar",
            "contentRule": "$.text",
            "postTimeRule": "$.create_timestamp",
            "diggRule": "$.digg_count",
            "replyCountRule": "$.reply_count",
            "replyListRule": "$.reply_list[*]",
            "replyNameRule": "$.user_info.user_name",
            "replyContentRule": "$.text",
            "authorRule": "$.user_info.is_author",
            "authorDiggRule": "$.has_author_digg",
            "replyAuthorRule": "$.user_info.is_author",
            "replyAuthorDiggRule": "$.has_author_digg",
            "imageRule": "$.image_url[*]"
        },
        "ruleParaReview": {
            "indexUrl": "comment/para/list?book_id={{$.data.data.novel_data.book_id}}&item_id={{$.data.data.novel_data.item_id}}&item_version={{$.data.data.novel_data.version}}",
            "indexListRule": "$.data.data.idea_data",
            "indexCountRule": "$.idea_count",
            "indexAuthorCommentedRule": "$.author_commented",
            "reviewUrl": "comment/para?book_id={{$.data.data.novel_data.book_id}}&item_id={{$.data.data.novel_data.item_id}}&para_index={{paraIndex}}&item_version={{$.data.data.novel_data.version}}&page={{page}}&count={{count}}&sort={{sort === 'hot' ? 'hot' : 'time_desc'}}",
            "listRule": "$.data.data.comments[*]",
            "totalRule": "$.data.data.count",
            "hasMoreRule": "$.data.data.has_more",
            "idRule": "$.comment_id",
            "nameRule": "$.user_info.user_name",
            "contentRule": "$.text",
            "postTimeRule": "$.create_timestamp",
            "diggRule": "$.digg_count",
            "replyCountRule": "$.reply_count",
            "replyListRule": "$.reply_list[*]",
            "replyNameRule": "$.user_info.user_name",
            "replyContentRule": "$.text",
            "authorRule": "$.user_info.is_author",
            "authorDiggRule": "$.has_author_digg",
            "replyAuthorRule": "$.user_info.is_author",
            "replyAuthorDiggRule": "$.has_author_digg",
            "imageRule": "$.image_url[*]"
        }
    })
}

impl TestServer {
    async fn start() -> Self {
        let upstream_url = start_upstream().await;

        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-review-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "v1-review-test-secret".to_string(),
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
        let token = common::login(&client, &base_url).await;

        // 书源走 v1（顺带覆盖 upsert 路径），书用 /reader3/saveBook 登记（测试种子）
        let resp = client
            .post(format!("{base_url}/v1/sources"))
            .bearer_auth(&token)
            .json(&book_source(&upstream_url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let payload: Value = resp.json().await.unwrap();
        let source_id = payload["id"]
            .as_str()
            .unwrap()
            .trim_start_matches("src_")
            .parse::<i64>()
            .unwrap();

        let resp = client
            .post(format!("{base_url}/reader3/saveBook"))
            .bearer_auth(&token)
            .json(&json!({
                "name": "测试书",
                "author": "作者",
                "bookUrl": format!("{upstream_url}/info?book_id={BOOK_ID}"),
                "bookSourceId": source_id,
                "tocUrl": format!("{upstream_url}/catalog?book_id={BOOK_ID}"),
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let payload: Value = resp.json().await.unwrap();
        let book_id = payload["data"]["bookId"].as_str().unwrap().to_string();

        // 章节登记表：index 0 → 上游正文地址（v1 以 index 定位章节）
        let pool = sqlx::SqlitePool::connect(&format!(
            "sqlite:{}?mode=rw",
            temp_dir.join("reader.db").display()
        ))
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO chapters (book_id, chapter_index, url, title) VALUES (?1, 0, ?2, '第一章')",
        )
        .bind(book_id.trim_start_matches("book_").parse::<i64>().unwrap())
        .bind(format!("{upstream_url}/content?item_id={ITEM_ID}"))
        .execute(&pool)
        .await
        .unwrap();

        Self {
            base_url,
            temp_dir,
            client,
            token,
            book_id,
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
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

#[tokio::test]
async fn chapter_comments_para_comments_and_index_flow() {
    let server = TestServer::start().await;

    // 章评：typed 形状 + 作者标记
    let resp = server
        .get(&format!("/v1/books/{}/chapters/0/comments", server.book_id))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], json!(true));
    assert_eq!(body["data"]["total"], json!(67));
    let item = &body["data"]["items"][0];
    assert_eq!(item["id"], json!("c1"));
    assert_eq!(item["name"], json!("读者甲"));
    assert_eq!(item["author"], json!(true));
    assert_eq!(item["authorDigg"], json!(true));
    assert_eq!(item["replies"][0]["name"], json!("读者乙"));

    // 段评：配图变体收口成单个 /reader3/image/<id>，且这张图能取回
    let resp = server
        .get(&format!(
            "/v1/books/{}/chapters/0/comments?para=0",
            server.book_id
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let images = body["data"]["items"][0]["images"].as_array().unwrap();
    assert_eq!(images.len(), 1, "变体应收口成单张");
    let route = images[0].as_str().unwrap();
    assert!(route.starts_with("/reader3/image/"), "收口地址: {route}");
    let image_resp = server
        .client
        .get(format!("{}{route}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(image_resp.status(), StatusCode::OK);
    assert_eq!(image_resp.headers()["content-type"], "image/jpeg");

    // sort 参数真的进了上游 URL（time 序返回换过内容的评论）
    let resp = server
        .get(&format!(
            "/v1/books/{}/chapters/0/comments?para=0&sort=time",
            server.book_id
        ))
        .await;
    let body: Value = resp.json().await.unwrap();
    // typed 模型的评论字段是 content（上游的 text 在解析时已归一）
    assert_eq!(
        body["data"]["items"][0]["content"],
        json!("最新序段评"),
        "完整响应: {body}"
    );

    // 段评概览：负数哨兵桶被过滤
    let resp = server
        .get(&format!(
            "/v1/books/{}/chapters/0/comments/index",
            server.book_id
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], json!(true));
    let paras = body["data"]["paras"].as_array().unwrap();
    assert_eq!(paras.len(), 2, "负数桶（-1）不进概览");
    assert_eq!(paras[0]["paraIndex"], json!(0));
    assert_eq!(paras[0]["count"], json!(140));
    assert_eq!(paras[0]["authorCommented"], json!(true));

    // para 取值非法：400
    let resp = server
        .get(&format!(
            "/v1/books/{}/chapters/0/comments?para=-3",
            server.book_id
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("MALFORMED_REQUEST"));

    server.cleanup();
}

#[tokio::test]
async fn chapter_images_endpoint_registers_pipeline_ids() {
    let server = TestServer::start().await;

    let resp = server
        .get(&format!("/v1/books/{}/chapters/0/images", server.book_id))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["enabled"], json!(true));
    let image = &body["images"][0];
    assert_eq!(image["caption"], json!("配图（画师：奈月Oo）"));
    assert_eq!(image["paraIndex"], json!(2));
    let route = image["url"].as_str().unwrap();
    assert!(
        route.starts_with("/reader3/image/"),
        "签名上游地址应收口成图片管道 id: {route}"
    );
    let image_resp = server
        .client
        .get(format!("{}{route}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(image_resp.status(), StatusCode::OK);
    assert_eq!(image_resp.headers()["content-type"], "image/jpeg");

    server.cleanup();
}

#[tokio::test]
async fn comments_require_auth_and_404_unknown_book() {
    let server = TestServer::start().await;

    let resp = server
        .client
        .get(format!(
            "{}/v1/books/{}/chapters/0/comments",
            server.base_url, server.book_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");

    let resp = server
        .get("/v1/books/book_99999999/chapters/0/comments")
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("BOOK_NOT_FOUND"));

    // 登记表为空的书（无该章）：404 CHAPTER_NOT_FOUND
    let resp = server
        .get("/v1/books/book_99999999/chapters/5/comments")
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    server.cleanup();
}
