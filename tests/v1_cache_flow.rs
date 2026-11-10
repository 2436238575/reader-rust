//! `/v1/cache`：占用统计（ETag/304）、按层/全部清理、书籍级清理。

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
    pool: sqlx::SqlitePool,
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = common::base_config(&temp_dir, "v1-cache-test-secret");
        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let (base_url, _server) = common::spawn_app(app).await;
        let client = reqwest::Client::new();
        let token = common::login(&client, &base_url).await;
        let pool = sqlx::SqlitePool::connect(&format!(
            "sqlite:{}?mode=rw",
            temp_dir.join("reader.db").display()
        ))
        .await
        .unwrap();
        Self {
            base_url,
            temp_dir,
            client,
            token,
            pool,
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    /// 预置一本网络书 + 章节登记行 + 完整目录状态（沿用 books 流的种子口径）。
    async fn seed_book_with_chapters(&self, url: &str, count: usize) -> String {
        let source_id: i64 = sqlx::query_scalar(
            "INSERT INTO book_sources (book_source_url, book_source_name, json, updated_at) \
             VALUES (?1, '测试源', '{}', 0) RETURNING book_source_id",
        )
        .bind(url)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let book_id: i64 = sqlx::query_scalar(
            "INSERT INTO books (book_url, book_source_id, name, author, created_at, updated_at) \
             VALUES (?1, ?2, '测试书', '作者', 0, 0) RETURNING book_id",
        )
        .bind(format!("{url}/book"))
        .bind(source_id)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        for index in 0..count {
            sqlx::query(
                "INSERT INTO chapters (book_id, chapter_index, url, title) VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(book_id)
            .bind(index as i64)
            .bind(format!("{url}/ch/{index}"))
            .bind(format!("第 {index} 章"))
            .execute(&self.pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO book_toc_state (book_id, complete, fetched_at) VALUES (?1, 1, ?2)",
        )
        .bind(book_id)
        .bind(reader_rust::util::time::now_ts())
        .execute(&self.pool)
        .await
        .unwrap();
        format!("book_{book_id}")
    }

    /// 预置一章上游正文（正文池，撑起 content 统计）。
    async fn seed_upstream_content(&self, book_id: &str, index: i32, url: &str, content: &str) {
        let state = build_state(common::base_config(&self.temp_dir, "v1-cache-test-secret"))
            .await
            .unwrap();
        state
            .chapter_content_service
            .save_upstream(
                reader_rust::service::chapter_content_service::UpstreamChapterMeta {
                    book_id,
                    chapter_url: url,
                    book_source_id: None,
                    book_source_name: "",
                    chapter_index: index,
                    chapter_title: "",
                    book_url: "",
                    fetch_url: url,
                },
                content,
            )
            .await
            .unwrap();
    }

    async fn delete(&self, path: &str) -> reqwest::Response {
        self.client
            .delete(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
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
async fn cache_requires_auth_with_problem_401() {
    let server = TestServer::start().await;
    for path in ["/v1/cache/stats", "/v1/cache", "/v1/books/book_1/cache"] {
        let resp = server
            .client
            .get(format!("{}{path}", server.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(resp.headers()["content-type"], "application/problem+json");
    }
    server.cleanup();
}

#[tokio::test]
async fn cache_stats_shape_and_revalidation() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 2)
        .await;
    server
        .seed_upstream_content(&book_id, 0, "https://up.example.com/ch/0", "第一章正文。")
        .await;

    let resp = server.get("/v1/cache/stats").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["cache-control"], "private, no-cache");
    let etag = resp.headers()["etag"].to_str().unwrap().to_string();
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("isSuccess").is_none(), "v1 响应不带包装");
    assert_eq!(body["content"]["chapters"], json!(1), "正文池一章");
    assert_eq!(body["content"]["versions"], json!(1));
    for layer in ["cover", "chapterList", "searchResults", "review"] {
        assert!(body[layer]["files"].is_u64(), "{layer} 应有 files 计数");
        assert!(body[layer]["bytes"].is_u64(), "{layer} 应有 bytes 统计");
    }

    // ETag 再验证
    let resp = server
        .client
        .get(format!("{}/v1/cache/stats", server.base_url))
        .bearer_auth(&server.token)
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);

    server.cleanup();
}

#[tokio::test]
async fn purge_cache_by_kind_and_all() {
    let server = TestServer::start().await;

    // 非法 kind：400 problem
    let resp = server.delete("/v1/cache?kind=toc").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("MALFORMED_REQUEST"));

    // 按层清理
    let resp = server.delete("/v1/cache?kind=review").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    for key in [
        "content",
        "cover",
        "chapterList",
        "searchResults",
        "review",
        "invalidSources",
    ] {
        assert!(
            body["purged"][key].is_u64(),
            "purged.{key} 应是数字: {body}"
        );
    }

    // 全部层
    let resp = server.delete("/v1/cache").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert!(body["purged"]["cover"].is_u64());

    server.cleanup();
}

#[tokio::test]
async fn delete_book_cache_resets_toc_state_and_keeps_registration() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 2)
        .await;
    let row_id: i64 = book_id.trim_start_matches("book_").parse().unwrap();

    let resp = server.delete(&format!("/v1/books/{book_id}/cache")).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 目录抓取状态被重置，登记行保留（与 /reader3 deleteBookCache 同语义）
    let state_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM book_toc_state WHERE book_id = ?1")
            .bind(row_id)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(state_rows, 0, "抓取状态应被清掉");
    let chapters: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chapters WHERE book_id = ?1")
        .bind(row_id)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(chapters, 2, "章节登记行保留");

    // 幂等：再删仍 204
    let resp = server.delete(&format!("/v1/books/{book_id}/cache")).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 未知书：404 精确码
    let resp = server.delete("/v1/books/book_99999999/cache").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("BOOK_NOT_FOUND"));

    server.cleanup();
}
