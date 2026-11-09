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
    /// 正文池服务：content 用例的预置写入用。
    content: std::sync::Arc<reader_rust::service::chapter_content_service::ChapterContentService>,
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-books-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = common::base_config(&temp_dir, "v1-books-test-secret");
        let state = build_state(cfg).await.unwrap();
        let content = state.chapter_content_service.clone();
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
            content,
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

    /// 预置一章上游正文（正文池，网络书读取链路的输入）。
    async fn seed_upstream_content(&self, book_id: &str, index: i32, url: &str, content: &str) {
        self.content
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
        .upload_local_book(
            "本地测试",
            "第一章 开始
正文内容甲。
第二章 后来
正文内容乙。
",
        )
        .await;

    let resp = server.get(&format!("/v1/books/{book_id}/chapters")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "TXT 按章节标题切分：{body}");
    assert_eq!(items[0]["title"], json!("第一章 开始"));
    assert_eq!(items[0]["index"], json!(0));
    assert_eq!(items[0]["titleOverridden"], json!(false));

    server.cleanup();
}

#[tokio::test]
async fn content_reads_via_index_with_version_etag() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 2)
        .await;
    server
        .seed_upstream_content(
            &book_id,
            0,
            "https://up.example.com/ch/0",
            "第一章的纯文本正文。",
        )
        .await;

    let resp = server
        .get(&format!("/v1/books/{book_id}/chapters/0/content"))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp.headers()["etag"].to_str().unwrap().to_string();
    assert!(etag.starts_with("\"cv_"), "ETag 应是版本标签: {etag}");
    let body: Value = resp.json().await.unwrap();
    assert!(
        body.get("isSuccess").is_none(),
        "v1 响应不带 ApiResponse 包装"
    );
    assert_eq!(body["bookId"], json!(book_id));
    assert_eq!(body["index"], json!(0));
    assert_eq!(body["format"], json!("text"), "纯文本正文应判为 text");
    assert_eq!(body["body"], json!("第一章的纯文本正文。"));
    assert!(body["version"]["id"].as_str().unwrap().starts_with("cv_"));
    assert_eq!(body["version"]["kind"], json!("upstream"));
    assert_eq!(body["selectionSource"], json!("upstream"));
    assert_eq!(body["stale"], json!(false));

    // If-None-Match 命中当前版本：304，客户端用缓存正文
    let not_modified = server
        .client
        .get(format!(
            "{}/v1/books/{book_id}/chapters/0/content",
            server.base_url
        ))
        .bearer_auth(&server.token)
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);

    server.cleanup();
}

#[tokio::test]
async fn content_detects_html_format_and_maps_fields() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 1)
        .await;
    server
        .seed_upstream_content(
            &book_id,
            0,
            "https://up.example.com/ch/0",
            "<p>富文本段落。</p><p>第二段。</p>",
        )
        .await;

    let resp = server
        .get(&format!(
            "/v1/books/{book_id}/chapters/0/content?variant=upstream"
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["format"], json!("html"), "含 <p> 的正文应判为 html");
    assert_eq!(body["selectionSource"], json!("upstream_preview"));

    server.cleanup();
}

#[tokio::test]
async fn content_index_beyond_registry_gets_chapter_404() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 2)
        .await;

    // index 超出登记表：抓取路径也拿不到（上游不可达）→ 章节级 404
    let resp = server
        .get(&format!("/v1/books/{book_id}/chapters/9/content"))
        .await;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "章节不存在应 404: {status} {body}"
    );
    assert_eq!(body["code"], json!("CHAPTER_NOT_FOUND"));

    server.cleanup();
}

#[tokio::test]
async fn content_empty_url_chapter_gets_chapter_404() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 2)
        .await;
    // 个别书源的章节没有独立 URL：登记表里有行，但正文池没有可定位的键
    sqlx::query("UPDATE chapters SET url = '' WHERE book_id = 1 AND chapter_index = 1")
        .execute(&server.pool)
        .await
        .unwrap();

    let resp = server
        .get(&format!("/v1/books/{book_id}/chapters/1/content"))
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("CHAPTER_NOT_FOUND"));

    server.cleanup();
}

#[tokio::test]
async fn content_malformed_variant_gets_400() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 1)
        .await;

    let resp = server
        .get(&format!(
            "/v1/books/{book_id}/chapters/0/content?variant=bogus"
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("MALFORMED_REQUEST"));

    server.cleanup();
}

#[tokio::test]
async fn local_book_content_reads_from_file() {
    let server = TestServer::start().await;
    let book_id = server
        .upload_local_book(
            "正文读取",
            "第一章 开始
正文内容甲。
第二章 后来
正文内容乙。
",
        )
        .await;

    let resp = server
        .get(&format!("/v1/books/{book_id}/chapters/0/content"))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["format"], json!("text"));
    let body_text = body["body"].as_str().unwrap();
    assert!(
        body_text.contains("正文内容甲"),
        "本地正文来自文件: {body_text}"
    );

    server.cleanup();
}

#[tokio::test]
async fn put_shelf_enrolls_then_is_idempotent() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 3)
        .await;

    // 首次入架：上游不可达也不阻塞（登记行信息兜底）
    let first = server
        .client
        .put(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let body: Value = first.json().await.unwrap();
    assert_eq!(body["bookId"], json!(book_id));

    // 重复入架：200 幂等
    let second = server
        .client
        .put(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    // 未知书：404 精确码
    let missing = server
        .client
        .put(format!("{}/v1/me/shelf/book_999", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let body: Value = missing.json().await.unwrap();
    assert_eq!(body["code"], json!("BOOK_NOT_FOUND"));

    server.cleanup();
}

#[tokio::test]
async fn patch_shelf_progress_updates_and_validates() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 5)
        .await;

    // 未入架先 PATCH：409 BOOK_NOT_IN_SHELF
    let before = server
        .client
        .patch(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({"progress": {"chapterIndex": 2, "offset": 0.5}}))
        .send()
        .await
        .unwrap();
    assert_eq!(before.status(), StatusCode::CONFLICT);
    let body: Value = before.json().await.unwrap();
    assert_eq!(body["code"], json!("BOOK_NOT_IN_SHELF"));

    // 入架 → 进度 PATCH：204；标题缺省从登记表解析
    server
        .client
        .put(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    let patched = server
        .client
        .patch(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({"progress": {"chapterIndex": 2, "offset": 0.5}}))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::NO_CONTENT);

    // 交叉验证：/reader3 读侧看到同样的进度（offset 0.5 → 万分比 5000）
    let shelf: Value = server
        .client
        .get(format!("{}/reader3/getBookshelf", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entry = &shelf["data"][0];
    assert_eq!(entry["durChapterIndex"], json!(2));
    assert_eq!(entry["durChapterPos"], json!(5000));
    assert_eq!(entry["durChapterTitle"], json!("第 2 章"));

    // offset 越界钳制到 0..1
    let clamped = server
        .client
        .patch(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({"progress": {"chapterIndex": 3, "offset": 1.7}}))
        .send()
        .await
        .unwrap();
    assert_eq!(clamped.status(), StatusCode::NO_CONTENT);
    let shelf: Value = server
        .client
        .get(format!("{}/reader3/getBookshelf", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(shelf["data"][0]["durChapterPos"], json!(10000));

    // 未知字段：422 UNKNOWN_FIELD
    let unknown = server
        .client
        .patch(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({"bookUrl": "https://x.example.com"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = unknown.json().await.unwrap();
    assert_eq!(body["code"], json!("UNKNOWN_FIELD"));

    server.cleanup();
}

#[tokio::test]
async fn delete_shelf_is_idempotent_and_keeps_registration() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 2)
        .await;

    server
        .client
        .put(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();

    let deleted = server
        .client
        .delete(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    // 幂等：再删仍 204（删除的不是登记行，章节列表仍可读）
    let again = server
        .client
        .delete(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::NO_CONTENT);
    let chapters = server.get(&format!("/v1/books/{book_id}/chapters")).await;
    assert_eq!(chapters.status(), StatusCode::OK);

    server.cleanup();
}

#[tokio::test]
async fn patch_book_updates_registration_fields() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 2)
        .await;

    let resp = server
        .client
        .patch(format!("{}/v1/books/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({"name": "改名后的书", "author": "新作者"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT, "PATCH 应成功");

    let detail = server.get(&format!("/v1/books/{book_id}")).await;
    assert_eq!(detail.status(), StatusCode::OK);
    let body: Value = detail.json().await.unwrap();
    assert_eq!(body["name"], json!("改名后的书"));
    assert_eq!(body["author"], json!("新作者"));
    assert_eq!(body["id"], json!(book_id));
    assert_eq!(body["totalChapters"], json!(2));
    assert_eq!(body["lastChapter"]["index"], json!(1));

    // 未知字段 422
    let unknown = server
        .client
        .patch(format!("{}/v1/books/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({"bookUrl": "https://x.example.com"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = unknown.json().await.unwrap();
    assert_eq!(body["code"], json!("UNKNOWN_FIELD"));

    server.cleanup();
}

#[tokio::test]
async fn book_detail_gets_precise_404() {
    let server = TestServer::start().await;

    let resp = server.get("/v1/books/book_999").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("BOOK_NOT_FOUND"));

    server.cleanup();
}
