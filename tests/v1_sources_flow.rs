//! `/v1/sources` 读链路的集成测试：鉴权、游标分页、过滤、`limit` 校验、
//! 游标解码、ETag/304。

use axum::http::{header, StatusCode};
use serde_json::{json, Value};
use std::path::PathBuf;

use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;

mod common;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-sources-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = common::base_config(&temp_dir, "v1-sources-test-secret");
        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let (base_url, _server) = common::spawn_app(app).await;
        let client = reqwest::Client::new();
        let token = common::login(&client, &base_url).await;
        Self {
            base_url,
            temp_dir,
            client,
            token,
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    async fn seed_source(&self, name: &str, url: &str) -> i64 {
        common::save_source(
            &self.client,
            &self.base_url,
            &self.token,
            &json!({
                "bookSourceName": name,
                "bookSourceUrl": url,
                "bookSourceGroup": "测试组",
            }),
        )
        .await
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

#[tokio::test]
async fn list_returns_summaries_with_v1_shape() {
    let server = TestServer::start().await;
    let id = server.seed_source("源甲", "https://a.example.com").await;

    let resp = server.get("/v1/sources").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "application/json");
    let body: Value = resp.json().await.unwrap();
    assert!(
        body.get("isSuccess").is_none(),
        "v1 响应不带 ApiResponse 包装"
    );
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], json!(format!("src_{id}")));
    assert_eq!(items[0]["name"], json!("源甲"));
    assert_eq!(items[0]["url"], json!("https://a.example.com"));
    assert_eq!(items[0]["group"], json!("测试组"));
    assert_eq!(items[0]["enabled"], json!(true), "未显式关闭即启用");
    assert_eq!(items[0]["enabledExplore"], json!(true));
    assert!(body["nextCursor"].is_null(), "单页应无游标");
    assert_eq!(body["total"], json!(1));

    server.cleanup();
}

#[tokio::test]
async fn list_requires_auth_with_problem_401() {
    let server = TestServer::start().await;

    let resp = server
        .client
        .get(format!("{}/v1/sources", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("AUTH_EXPIRED"));

    server.cleanup();
}

#[tokio::test]
async fn cursor_pagination_walks_all_items_without_duplicates() {
    let server = TestServer::start().await;
    let mut seeded = Vec::new();
    for index in 0..5 {
        seeded.push(
            server
                .seed_source(
                    &format!("源{index}"),
                    &format!("https://s{index}.example.com"),
                )
                .await,
        );
    }
    seeded.sort();

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        pages += 1;
        let path = match &cursor {
            Some(cursor) => format!("/v1/sources?limit=2&cursor={cursor}"),
            None => "/v1/sources?limit=2".to_string(),
        };
        let body: Value = server.get(&path).await.json().await.unwrap();
        for item in body["items"].as_array().unwrap() {
            seen.push(item["id"].as_str().unwrap().to_string());
        }
        assert_eq!(body["total"], json!(5), "total 是过滤后的总数，不随页变");
        match body["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
        assert!(pages < 10, "分页不该超三页");
    }
    assert_eq!(pages, 3, "5 条按 2 条一页应为 3 页");
    let expected: Vec<String> = seeded.iter().map(|id| format!("src_{id}")).collect();
    assert_eq!(seen, expected, "游标遍历应覆盖全部且按 id 升序无重复");

    server.cleanup();
}

#[tokio::test]
async fn query_filter_matches_name_or_url() {
    let server = TestServer::start().await;
    server
        .seed_source("番茄源", "https://fanqie.example.com")
        .await;
    server
        .seed_source("起点源", "https://qidian.example.com")
        .await;

    // 按名称命中
    let body: Value = server.get("/v1/sources?q=番茄").await.json().await.unwrap();
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["name"], json!("番茄源"));

    // 按 URL 命中（大小写不敏感）
    let body: Value = server
        .get("/v1/sources?q=QIDIAN")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["total"], json!(1));
    assert_eq!(body["items"][0]["name"], json!("起点源"));

    // 无命中
    let body: Value = server
        .get("/v1/sources?q=不存在")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["total"], json!(0));
    assert!(body["items"].as_array().unwrap().is_empty());

    server.cleanup();
}

#[tokio::test]
async fn limit_bounds_and_invalid_cursor_are_errors() {
    let server = TestServer::start().await;

    for path in ["/v1/sources?limit=0", "/v1/sources?limit=201"] {
        let resp = server.get(path).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(resp.headers()["content-type"], "application/problem+json");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["code"], json!("MALFORMED_REQUEST"));
    }

    let resp = server.get("/v1/sources?cursor=not-a-cursor").await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("INVALID_CURSOR"));

    server.cleanup();
}

#[tokio::test]
async fn etag_matches_on_second_request_and_returns_304() {
    let server = TestServer::start().await;
    server.seed_source("源甲", "https://a.example.com").await;

    let resp = server.get("/v1/sources").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers()["etag"]
        .to_str()
        .expect("列表响应应带 ETag")
        .to_string();
    assert!(
        etag.starts_with('"') && etag.ends_with('"'),
        "ETag 格式: {etag}"
    );
    assert_eq!(
        resp.headers()["cache-control"],
        "private, no-cache",
        "私有缓存且每次重验证"
    );

    let not_modified = server
        .client
        .get(format!("{}/v1/sources", server.base_url))
        .bearer_auth(&server.token)
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
    let bytes = not_modified.bytes().await.unwrap();
    assert!(bytes.is_empty(), "304 body 必须为空");

    // 数据变化后 ETag 必须变化（新增一个源）
    server.seed_source("源乙", "https://b.example.com").await;
    let changed = server.get("/v1/sources").await;
    let new_etag = changed.headers()["etag"].to_str().unwrap();
    assert_ne!(new_etag, etag, "数据变化后 ETag 应不同");

    server.cleanup();
}
