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
        sqlx::query(
            "INSERT INTO book_toc_state (book_id, complete, fetched_at) VALUES (?1, 1, ?2)",
        )
        .bind(book_id)
        // 新鲜状态：第一页读路径按 TTL 判定（完整目录 1h），过期会真的回源，
        // 而这批用例只想验证登记表读链路
        .bind(reader_rust::util::time::now_ts())
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

    /// 预置一份用户编辑版本（基于当前最新上游，设为当前版本）。
    async fn seed_user_version(&self, book_id: &str, index: i32, url: &str, content: &str) {
        use reader_rust::storage::db::content_repo::RecordOutcome;
        let latest = self
            .content
            .latest(book_id, url)
            .await
            .unwrap()
            .expect("种子顺序：先写上游版本");
        let base = latest
            .latest_upstream_version_id
            .expect("种子顺序：先写上游版本");
        let outcome = self
            .content
            .save_user_version(
                "user_1",
                book_id,
                url,
                "edited",
                content,
                Some(base),
                index,
                "",
                "",
                "",
                true,
                None,
            )
            .await
            .unwrap();
        match outcome {
            RecordOutcome::Appended { .. } | RecordOutcome::Unchanged { .. } => {}
        }
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
    // 过渡期例外：url 只读出现在响应里，供阅读器调 /reader3 的评论/配图/编辑
    // 与 IndexedDB 缓存键；这些域迁走 v1 后删除（契约红线 6 例外）
    assert_eq!(
        items[0]["url"],
        json!("https://up.example.com/ch/0"),
        "过渡期保留只读 url 诊断字段"
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
    assert!(
        etag.len() == 34 && etag.starts_with('"') && etag.ends_with('"'),
        "ETag 应是响应体的强 md5 标签: {etag}"
    );
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
async fn content_etag_covers_diff_fields_not_only_version() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 1)
        .await;
    let url = "https://up.example.com/ch/0";
    server
        .seed_upstream_content(&book_id, 0, url, "上游第一版正文。")
        .await;
    server
        .seed_user_version(&book_id, 0, url, "用户编辑后的正文。")
        .await;

    // 第一次读：编辑版 + 内联 diff，基准是当前最新上游
    let resp = server
        .get(&format!("/v1/books/{book_id}/chapters/0/content"))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let etag_v1 = resp.headers()["etag"].to_str().unwrap().to_string();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["edited"], json!(true));
    assert!(body["diff"].is_array(), "编辑版应带内联 diff: {body}");
    let base_v1 = body["diffBaseVersionId"]
        .as_str()
        .expect("编辑版 diff 有基准版本")
        .to_string();

    // 上游推进一版：用户版本号不变（仍是 cv_x），但 diff 基准/差异内容变了
    server
        .seed_upstream_content(&book_id, 0, url, "上游第二版正文（新）。")
        .await;

    // 旧 ETag 必须失效——只盖版本号的 ETag 会在这里返回 304，把编辑视图
    // 永久钉在首次读取时的上游版本上
    let resp = server
        .client
        .get(format!(
            "{}/v1/books/{book_id}/chapters/0/content",
            server.base_url
        ))
        .bearer_auth(&server.token)
        .header(header::IF_NONE_MATCH, &etag_v1)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "diff 基准变了，旧 ETag 不能 304"
    );
    let etag_v2 = resp.headers()["etag"].to_str().unwrap().to_string();
    assert_ne!(etag_v1, etag_v2, "响应体变化必须换 ETag");
    let body: Value = resp.json().await.unwrap();
    let base_v2 = body["diffBaseVersionId"].as_str().unwrap();
    assert_ne!(base_v1, base_v2, "diffBaseVersionId 应跟到新的最新上游");
    assert_eq!(
        body["version"]["kind"],
        json!("edited"),
        "默认版本仍是用户编辑版"
    );

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

    // offset 越界：422 VALIDATION_FAILED 带 pointer（不做静默 clamp）
    let out_of_range = server
        .client
        .patch(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({"progress": {"chapterIndex": 3, "offset": 1.7}}))
        .send()
        .await
        .unwrap();
    assert_eq!(out_of_range.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = out_of_range.json().await.unwrap();
    assert_eq!(body["code"], json!("VALIDATION_FAILED"));
    assert_eq!(body["errors"][0]["pointer"], json!("/progress/offset"));

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

/// 节流语义的回归钉：「最近阅读时间」每次进度保存都刷新、60s 内节流。
/// 如果把它改回「只在换章时刷新」，这个用例会红——否则套件全绿，
/// 「同一章读两小时沉下去」的 bug 会原样回来。
#[tokio::test]
async fn patch_shelf_progress_refreshes_last_read_time_with_throttle() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 3)
        .await;

    server
        .client
        .put(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();

    let read_time = |pool: sqlx::SqlitePool| async move {
        let (time,): (Option<i64>,) = sqlx::query_as(
            "SELECT dur_chapter_time FROM shelf_books WHERE user_ns = 'user_1' AND book_id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        time
    };

    // 第一次保存：时间戳落为「现在」
    server
        .client
        .patch(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({"progress": {"chapterIndex": 1, "offset": 0.1}}))
        .send()
        .await
        .unwrap();
    let first = read_time(server.pool.clone())
        .await
        .expect("首次保存应写入时间");
    assert!(first > 0, "时间戳应是服务端时间: {first}");

    // 60s 窗口内同一章再保存：节流，不改动时间戳
    server
        .client
        .patch(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({"progress": {"chapterIndex": 1, "offset": 0.2}}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        read_time(server.pool.clone()).await,
        Some(first),
        "60s 内的重复保存应被节流（时间戳不变）"
    );

    // 把时间回拨到 120 秒前，再保存**同一章**：必须刷新——
    // 这是「不绑在换章上」的直接断言（换章绑定实现会保持旧值）
    sqlx::query(
        "UPDATE shelf_books SET dur_chapter_time = ?1 WHERE user_ns = 'user_1' AND book_id = 1",
    )
    .bind(reader_rust::util::time::now_ts() - 120)
    .execute(&server.pool)
    .await
    .unwrap();
    server
        .client
        .patch(format!("{}/v1/me/shelf/{book_id}", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({"progress": {"chapterIndex": 1, "offset": 0.3}}))
        .send()
        .await
        .unwrap();
    let refreshed = read_time(server.pool.clone()).await.expect("应仍有时间戳");
    // 回拨 120s 再保存同一章：时间戳必须回到「现在」（>= first）。
    // 换章绑定实现会保持回拨后的旧值（first - 120），这里会红。
    assert!(
        refreshed >= first,
        "同一章保存也必须刷新最近阅读时间（60s 窗口外）：回拨后仍为 {refreshed}，首次为 {first}"
    );

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
    // 登记行与章节登记表保留——清缓存只重置目录抓取状态。旧目录因此仍可读：
    // 上游地址不可达时也返回 200 + stale=true（后台刷新失败不影响本次读取）。
    let books: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM books")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    let chapters: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chapters")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(books, 1, "书籍登记行不随移出书架删除");
    assert_eq!(chapters, 2, "章节登记行不随移出书架删除");
    let resp = server.get(&format!("/v1/books/{book_id}/chapters")).await;
    assert_eq!(resp.status(), StatusCode::OK, "旧登记表应继续可读");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["stale"], json!(true), "状态被重置的目录应标 stale");

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
#[tokio::test]
async fn batch_content_collects_per_chapter_outcomes() {
    let server = TestServer::start().await;
    let book_id = server
        .seed_book_with_chapters("https://up.example.com", 4)
        .await;
    server
        .seed_upstream_content(&book_id, 0, "https://up.example.com/ch/0", "第 0 章正文。")
        .await;
    server
        .seed_upstream_content(
            &book_id,
            2,
            "https://up.example.com/ch/2",
            "<p>第二章富文本。</p>",
        )
        .await;

    // 合法批量：0 有正文、1 未缓存、2 富文本——逐项成败
    let resp = server
        .client
        .post(format!(
            "{}/v1/books/{book_id}/chapters:batchContent",
            server.base_url
        ))
        .bearer_auth(&server.token)
        .json(&json!({"indices": [0, 1, 2]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "整体恒 200");
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    // 顺序按请求回排（buffer_unordered 之后按下标归位）
    assert_eq!(items[0]["index"], json!(0));
    assert_eq!(items[0]["data"]["body"], json!("第 0 章正文。"));
    assert!(items[0]["data"]["version"]["id"]
        .as_str()
        .unwrap()
        .starts_with("cv_"));
    assert_eq!(items[1]["index"], json!(1));
    assert!(
        items[1]["error"]["code"].as_str().is_some(),
        "未缓存章节应带 error: {}",
        items[1]
    );
    assert_eq!(items[2]["data"]["format"], json!("html"));

    // 校验：空 / 超上限 / 重复 / 负数 → 400
    for payload in [
        json!({"indices": []}),
        json!({"indices": (0..11).collect::<Vec<i32>>()}),
        json!({"indices": [0, 0]}),
        json!({"indices": [-1]}),
    ] {
        let resp = server
            .client
            .post(format!(
                "{}/v1/books/{book_id}/chapters:batchContent",
                server.base_url
            ))
            .bearer_auth(&server.token)
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{payload}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["code"], json!("MALFORMED_REQUEST"));
    }

    server.cleanup();
}
