//! `/v1/books/{bookId}/chapters` 的集成测试：登记表读链路（对外 index 身份）、
//! 标题覆盖层、游标分页、ETag/304、404 与 limit/游标校验、本地书分支。

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
    /// 独立连到测试库的池：登记表/覆盖层的 SQL 预置用。
    pool: sqlx::SqlitePool,
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-books-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = common::base_config(&temp_dir, "v1-books-test-secret");
        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let (base_url, _server) = common::spawn_app(app.clone()).await;
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

    /// 预置一本网络书 + 章节登记行 + 完整目录状态。
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
        sqlx::query("INSERT INTO book_toc_state (book_id, complete, fetched_at) VALUES (?1, 1, 0)")
            .bind(book_id)
            .execute(&self.pool)
            .await
            .unwrap();
        format!("book_{book_id}")
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
    }

    /// 本地 TXT 书（走本地分支，不经登记表）。
    async fn upload_local_book(&self, name: &str, content: &str) -> String {
        let boundary = "----v1-books-test";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}.txt\"\r\nContent-Type: text/plain\r\n\r\n{content}\r\n--{boundary}--\r\n"
        );
        let resp = self
            .client
            .post(format!("{}/reader3/uploadTxtBook", self.base_url))
            .bearer_auth(&self.token)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "TXT 上传应成功");
        let payload: Value = resp.json().await.unwrap();
        payload["data"]["bookId"].as_str().unwrap().to_string()
    }
}

#[tokio::test]
async fn chapters_list_requires_auth_with_problem_401() {
    let server = TestServer::start().await;

    let resp = server
        .client
        .get(format!("{}/v1/books/book_1/chapters", server.base_url))
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
async fn unknown_book_gets_precise_404() {
    let server = TestServer::start().await;

    let resp = server.get("/v1/books/book_999/chapters").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["code"],
        json!("BOOK_NOT_FOUND"),
        "书籍不存在必须用精确码，不是通用 NOT_FOUND"
    );

    server.cleanup();
}

#[tokio::test]
async fn chapters_read_from_registry_with_index_identity() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 5)
        .await;

    let resp = server.get(&format!("/v1/books/{book_id}/chapters")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body.get("isSuccess").is_none(),
        "v1 响应不带 ApiResponse 包装"
    );
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    assert_eq!(items[0]["index"], json!(0));
    assert_eq!(items[0]["title"], json!("第 0 章"));
    assert_eq!(items[0]["titleOverridden"], json!(false));
    assert!(
        items[0].get("url").is_none(),
        "URL 不进 v1 响应（身份是 index）"
    );
    assert_eq!(body["total"], json!(5));
    assert!(body["nextCursor"].is_null());

    server.cleanup();
}

#[tokio::test]
async fn cursor_pagination_walks_without_duplicates() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 5)
        .await;

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            Some(cursor) => format!("/v1/books/{book_id}/chapters?limit=2&cursor={cursor}"),
            None => format!("/v1/books/{book_id}/chapters?limit=2"),
        };
        let body: Value = server.get(&path).await.json().await.unwrap();
        for item in body["items"].as_array().unwrap() {
            seen.push(item["index"].as_i64().unwrap());
        }
        match body["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(seen, vec![0, 1, 2, 3, 4], "游标遍历按 index 升序无重漏");

    server.cleanup();
}

#[tokio::test]
async fn title_override_is_flagged_and_changes_etag() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 3)
        .await;

    // 用户改名：写 chapter_title_overrides（与 /reader3/renameChapter 同一张表）
    sqlx::query("INSERT INTO chapter_title_overrides (book_id, chapter_url, title, updated_at) VALUES (1, 'https://up.example.com/ch/1', '改名后的第二章', 0)")
        .execute(&server.pool)
        .await
        .unwrap();

    let resp = server.get(&format!("/v1/books/{book_id}/chapters")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers()["etag"].to_str().unwrap().to_string();
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items[1]["title"], json!("改名后的第二章"));
    assert_eq!(items[1]["titleOverridden"], json!(true));
    assert_eq!(items[0]["titleOverridden"], json!(false));

    // 覆盖变化 → ETag 变化；未覆盖请求 → 304
    let not_modified = server
        .client
        .get(format!("{}/v1/books/{book_id}/chapters", server.base_url))
        .bearer_auth(&server.token)
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);

    sqlx::query("DELETE FROM chapter_title_overrides WHERE book_id = 1")
        .execute(&server.pool)
        .await
        .unwrap();
    let after = server.get(&format!("/v1/books/{book_id}/chapters")).await;
    assert_ne!(
        after.headers()["etag"].to_str().unwrap(),
        etag,
        "覆盖删除后 ETag 应变化"
    );
    let body: Value = after.json().await.unwrap();
    assert_eq!(body["items"][1]["titleOverridden"], json!(false));

    server.cleanup();
}

#[tokio::test]
async fn limit_bounds_and_invalid_cursor() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 3)
        .await;

    for path in [
        &format!("/v1/books/{book_id}/chapters?limit=0"),
        &format!("/v1/books/{book_id}/chapters?limit=201"),
    ] {
        let resp = server.get(path).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{path}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["code"], json!("MALFORMED_REQUEST"));
    }

    let resp = server
        .get(&format!("/v1/books/{book_id}/chapters?cursor=bogus"))
        .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("INVALID_CURSOR"));

    server.cleanup();
}

/// 本地书分支：TXT 不写登记表，目录由文件确定性生成，index 直接可用。
#[tokio::test]
async fn local_txt_book_chapters_come_from_the_local_branch() {
    let server = TestServer::start().await;
    let book_id = server
        .upload_local_book("本地测试", "第一章 内容甲\n第二章 内容乙\n第三章 内容丙\n")
        .await;

    let resp = server.get(&format!("/v1/books/{book_id}/chapters")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "TXT 按空行/章节标题切分：{body}");
    assert_eq!(items[0]["index"], json!(0));
    assert_eq!(items[0]["titleOverridden"], json!(false));

    server.cleanup();
}
