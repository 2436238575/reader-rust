//! 章节改名（持久覆盖层）的端到端行为：
//! - 改名后 getChapterList 返回新标题，ETag 随之变化；
//! - `refresh=1` / 目录缓存重写后覆盖仍在（读取时叠加，不落缓存文件）；
//! - 恢复原名（title=null / 与原标题相同）删除覆盖行；
//! - 版本时间线保留各版本抓取时的标题快照，不随改名变化；
//! - 整本 TXT 导入联动：preview 标出将改名的章，导入把 TXT 标题写进覆盖层；
//! - 全文检索命中条目的标题与目录显示一致。

mod common;
use axum::http::{header, HeaderValue, StatusCode};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use reader_rust::service::chapter_content_service::{ChapterContentService, UpstreamChapterMeta};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
    content: Arc<ChapterContentService>,
    /// 独立连到测试库的池：目录登记表等 DB 侧预置用。
    pool: sqlx::SqlitePool,
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-rename-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "rename-test-secret".to_string(),
            request_timeout_secs: 10,
            admin_username: "reader1".to_string(),
            admin_password: "password123".to_string(),
            rate_limit_disabled: true,
            allow_private_network: true,
            ..AppConfig::default()
        };

        let state = build_state(cfg).await.unwrap();
        let content = state.chapter_content_service.clone();
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
            .post(format!("{base_url}/v1/auth/login"))
            .header("x-requested-with", "XMLHttpRequest")
            .json(&json!({ "username": "reader1", "password": "password123" }))
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        let token = payload["accessToken"].as_str().unwrap().to_string();
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
            content,
            pool,
        }
    }

    /// 预置目录登记行：chapters 表 + book_toc_state（完整目录）。
    async fn seed_toc(&self, book_id: &str, chapters: &[(&str, &str)]) {
        let id: i64 = book_id.trim_start_matches("book_").parse().unwrap();
        let mut tx = self.pool.begin().await.unwrap();
        // 整份重写语义：先清旧行（模拟上游重抓整表覆盖）
        sqlx::query("DELETE FROM chapters WHERE book_id = ?1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
        for (index, (title, url)) in chapters.iter().enumerate() {
            sqlx::query(
                "INSERT INTO chapters (book_id, chapter_index, url, title) VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(id)
            .bind(index as i64)
            .bind(url)
            .bind(title)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO book_toc_state (book_id, complete, fetched_at) VALUES (?1, 1, ?2)              ON CONFLICT(book_id) DO UPDATE SET complete = 1, fetched_at = excluded.fetched_at",
        )
        .bind(id)
        .bind(reader_rust::util::time::now_ts())
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    async fn seed_chapter_cache(&self, book_id: &str, chapter_url: &str, content: &str) {
        self.content
            .save_upstream(
                UpstreamChapterMeta {
                    book_id,
                    chapter_url,
                    book_source_id: None,
                    book_source_name: "",
                    chapter_index: 0,
                    chapter_title: "",
                    book_url: "",
                    fetch_url: chapter_url,
                },
                content,
            )
            .await
            .unwrap();
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let (status, payload) = self.post_raw(path, body).await;
        assert_eq!(status, StatusCode::OK, "{path} 应返回 200");
        assert_eq!(
            payload["isSuccess"],
            json!(true),
            "{path} 应成功: {payload}"
        );
        payload["data"].clone()
    }

    async fn post_raw(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let resp = self
            .client
            .post(format!("{}/reader3/{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        (status, resp.json().await.unwrap())
    }

    async fn upload_txt(&self, name: &str, content: &str) -> Value {
        let boundary = "----rename-test-upload";
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
        let payload: Value = resp.json().await.unwrap();
        assert_eq!(payload["isSuccess"], json!(true), "上传应成功: {payload}");
        payload["data"].clone()
    }

    async fn import_multipart(&self, fields: &[(&str, &str)], file: &str) -> Value {
        let boundary = "----rename-test-import";
        let mut body = String::new();
        for (name, value) in fields {
            body.push_str(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            ));
        }
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"import.txt\"\r\nContent-Type: text/plain\r\n\r\n{file}\r\n"
        ));
        body.push_str(&format!("--{boundary}--\r\n"));
        let resp = self
            .client
            .post(format!("{}/reader3/importChapterText", self.base_url))
            .bearer_auth(&self.token)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        assert_eq!(payload["isSuccess"], json!(true), "导入应成功: {payload}");
        payload["data"].clone()
    }

    async fn preview_multipart(&self, fields: &[(&str, &str)], file: &str) -> Value {
        let boundary = "----rename-test-import";
        let mut body = String::new();
        for (name, value) in fields {
            body.push_str(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            ));
        }
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"import.txt\"\r\nContent-Type: text/plain\r\n\r\n{file}\r\n"
        ));
        body.push_str(&format!("--{boundary}--\r\n"));
        let resp = self
            .client
            .post(format!("{}/reader3/previewChapterImport", self.base_url))
            .bearer_auth(&self.token)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        assert_eq!(payload["isSuccess"], json!(true), "预览应成功: {payload}");
        payload["data"].clone()
    }

    /// getChapterList，返回（状态码，ETag，data）。
    async fn chapter_list_with_etag(
        &self,
        book_id: &str,
        refresh: bool,
        if_none_match: Option<&str>,
    ) -> (StatusCode, Option<String>, Value) {
        let mut request = self
            .client
            .get(format!("{}/reader3/getChapterList", self.base_url))
            .bearer_auth(&self.token)
            .query(&[("bookId", book_id)]);
        if refresh {
            request = request.query(&[("refresh", "1")]);
        }
        if let Some(etag) = if_none_match {
            request = request.header(header::IF_NONE_MATCH, etag);
        }
        let resp = request.send().await.unwrap();
        let status = resp.status();
        let etag = resp
            .headers()
            .get(header::ETAG)
            .map(HeaderValue::to_str)
            .map(|result| result.unwrap().to_string());
        let payload: Value = resp.json().await.unwrap();
        (status, etag, payload["data"].clone())
    }

    /// 登记一本书（resolve_book 校验需要 books 行存在）：
    /// 用本地 TXT 上传登记最省事（不需要书源），目录缓存另行 seed。
    async fn register_placeholder_book(&self, name: &str) -> String {
        let book = self.upload_txt(name, "占位\n").await;
        book["bookId"].as_str().unwrap().to_string()
    }

    async fn chapter_list(&self, book_id: &str) -> Value {
        let (_, _, data) = self.chapter_list_with_etag(book_id, false, None).await;
        data
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

const BOOK_TEXT: &str = "第一章 开始\n原始正文。\n第二章 继续\n第二章正文。\n";

#[tokio::test]
async fn rename_updates_chapter_list_and_etag() {
    let server = TestServer::start().await;
    let book = server.upload_txt("改名测试", BOOK_TEXT).await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let chapters = server.chapter_list(&book_id).await;
    let chapter_url = chapters[0]["url"].as_str().unwrap().to_string();

    let (_, etag_before, _) = server.chapter_list_with_etag(&book_id, false, None).await;

    let result = server
        .post(
            "renameChapter",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "title": "第一章 新开始",
            }),
        )
        .await;
    assert_eq!(result["renamed"], json!(true));
    assert_eq!(result["hasOverride"], json!(true));
    assert_eq!(result["originalTitle"], json!("第一章 开始"));

    // 列表立即生效（覆盖层叠加 + 乐观语义），ETag 随标题变化
    let (status, etag_after, list) = server.chapter_list_with_etag(&book_id, false, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["title"], json!("第一章 新开始"));
    assert_eq!(list[1]["title"], json!("第二章 继续"));
    assert_ne!(etag_after, etag_before, "标题变了 ETag 必须变");

    // 旧 ETag 的条件请求不再 304
    let (status, _, _) = server
        .chapter_list_with_etag(&book_id, false, etag_before.as_deref())
        .await;
    assert_eq!(status, StatusCode::OK, "改名后旧 ETag 不应命中 304");
}

#[tokio::test]
async fn rename_survives_toc_refresh_and_cache_rewrite() {
    let server = TestServer::start().await;
    // 非本地书：目录读取走 toc 缓存文件（本地书有自己的章节表，不经缓存）
    server
        .post(
            "saveBookSource",
            json!({
                "bookSourceName": "假源",
                "bookSourceUrl": "http://up.invalid",
                "enabled": true,
            }),
        )
        .await;
    let sources = server.post("getBookSources", json!({})).await;
    let source_id = sources[0]["bookSourceId"].as_i64().unwrap();
    let book = server
        .post(
            "saveBook",
            json!({
                "name": "网络书",
                "author": "作者",
                "bookUrl": "http://up.invalid/info",
                "bookSourceId": source_id,
                "tocUrl": "http://up.invalid/toc",
            }),
        )
        .await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    server
        .seed_toc(
            &book_id,
            &[
                ("第一章 开始", "http://up/ch/1"),
                ("第二章 继续", "http://up/ch/2"),
            ],
        )
        .await;
    let chapter_url = "http://up/ch/1";
    server
        .post(
            "renameChapter",
            json!({ "bookId": book_id, "chapterUrl": chapter_url, "title": "第一章 楔子" }),
        )
        .await;
    let (_, _, list) = server.chapter_list_with_etag(&book_id, false, None).await;
    assert_eq!(list[0]["title"], json!("第一章 楔子"));

    // 模拟上游重抓整份重写目录缓存：同 URL 不同标题重写缓存文件后，
    // 覆盖层必须按 url 重新套用（真实链路里 refresh=1 重抓后走同一读路径）
    server
        .seed_toc(
            &book_id,
            &[
                ("第一章 上游重抓标题", "http://up/ch/1"),
                ("第二章 继续", "http://up/ch/2"),
            ],
        )
        .await;
    let (_, _, list) = server.chapter_list_with_etag(&book_id, false, None).await;
    assert_eq!(
        list[0]["title"],
        json!("第一章 楔子"),
        "重抓后覆盖必须按 url 重新套用"
    );
    assert_eq!(list[1]["title"], json!("第二章 继续"));
}

#[tokio::test]
async fn restore_original_title_and_idempotent_same_name() {
    let server = TestServer::start().await;
    let book = server.upload_txt("恢复测试", BOOK_TEXT).await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let chapters = server.chapter_list(&book_id).await;
    let chapter_url = chapters[0]["url"].as_str().unwrap().to_string();

    // 改名 → 状态查询有覆盖
    server
        .post(
            "renameChapter",
            json!({ "bookId": book_id, "chapterUrl": chapter_url, "title": "第一章 改名了" }),
        )
        .await;
    let state = server
        .post(
            "getChapterTitleOverride",
            json!({ "bookId": book_id, "chapterUrl": chapter_url }),
        )
        .await;
    assert_eq!(state["hasOverride"], json!(true));
    assert_eq!(state["title"], json!("第一章 改名了"));
    assert_eq!(state["originalTitle"], json!("第一章 开始"));

    // 恢复原名（title=null）→ 目录回原标题，覆盖行删除
    let result = server
        .post(
            "renameChapter",
            json!({ "bookId": book_id, "chapterUrl": chapter_url, "title": Value::Null }),
        )
        .await;
    assert_eq!(result["renamed"], json!(false));
    let list = server.chapter_list(&book_id).await;
    assert_eq!(list[0]["title"], json!("第一章 开始"));
    let state = server
        .post(
            "getChapterTitleOverride",
            json!({ "bookId": book_id, "chapterUrl": chapter_url }),
        )
        .await;
    assert_eq!(state["hasOverride"], json!(false));
    assert_eq!(state["title"], json!("第一章 开始"));

    // 改回原标题：幂等处理为「恢复」，不残留覆盖行
    server
        .post(
            "renameChapter",
            json!({ "bookId": book_id, "chapterUrl": chapter_url, "title": "第一章 开始" }),
        )
        .await;
    let state = server
        .post(
            "getChapterTitleOverride",
            json!({ "bookId": book_id, "chapterUrl": chapter_url }),
        )
        .await;
    assert_eq!(state["hasOverride"], json!(false));
}

#[tokio::test]
async fn rename_rejects_bad_input() {
    let server = TestServer::start().await;
    let book = server.upload_txt("校验测试", BOOK_TEXT).await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let chapters = server.chapter_list(&book_id).await;
    let chapter_url = chapters[0]["url"].as_str().unwrap().to_string();

    // 缺 chapterUrl → 400
    let (status, _) = server
        .post_raw(
            "renameChapter",
            json!({ "bookId": book_id, "title": "新名字" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 超过 100 字符 → 400
    let long_title = "标".repeat(101);
    let (status, _) = server
        .post_raw(
            "renameChapter",
            json!({ "bookId": book_id, "chapterUrl": chapter_url, "title": long_title }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn timeline_keeps_original_title_snapshot() {
    let server = TestServer::start().await;
    let book = server.upload_txt("历史快照测试", BOOK_TEXT).await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let chapters = server.chapter_list(&book_id).await;
    let chapter_url = chapters[0]["url"].as_str().unwrap().to_string();

    // 先产生一个版本（标题快照 = 原标题），再改名
    server
        .post(
            "saveChapterEdit",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "chapterIndex": 0,
                "chapterTitle": "第一章 开始",
                "content": "编辑后的正文。",
            }),
        )
        .await;
    server
        .post(
            "renameChapter",
            json!({ "bookId": book_id, "chapterUrl": chapter_url, "title": "第一章 新开始" }),
        )
        .await;

    let timeline = server
        .post(
            "getChapterTimeline",
            json!({ "bookId": book_id, "chapterUrl": chapter_url }),
        )
        .await;
    let items = timeline["items"].as_array().unwrap();
    assert!(!items.is_empty());
    for item in items {
        assert_eq!(
            item["chapterTitle"],
            json!("第一章 开始"),
            "时间线标题快照不随改名变化"
        );
    }
}

#[tokio::test]
async fn txt_import_applies_titles_with_toggle() {
    let server = TestServer::start().await;
    // 书 A：默认应用标题
    let book = server
        .upload_txt("导入改名A", "第一章 甲\n正文甲。\n第二章 乙\n正文乙。\n")
        .await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let import_text = "第一章 新甲\n导入正文甲。\n第二章 新乙\n导入正文乙。\n";

    // 预览：标题按序号匹配上，但 TXT 标题与目录不同 → titleChanged
    let preview = server
        .preview_multipart(
            &[("bookId", book_id.as_str()), ("mode", "book")],
            import_text,
        )
        .await;
    let preview_chapters = preview["chapters"].as_array().unwrap();
    assert_eq!(preview_chapters.len(), 2);
    assert_eq!(preview_chapters[0]["titleChanged"], json!(true));
    assert_eq!(preview_chapters[0]["currentTitle"], json!("第一章 甲"));
    assert_eq!(preview_chapters[1]["titleChanged"], json!(true));

    let result = server
        .import_multipart(
            &[
                ("bookId", book_id.as_str()),
                ("mode", "book"),
                ("selectedIndexes", "0,1"),
            ],
            import_text,
        )
        .await;
    assert_eq!(result["titled"], json!(2), "应写入 2 条标题覆盖: {result}");
    let list = server.chapter_list(&book_id).await;
    assert_eq!(list[0]["title"], json!("第一章 新甲"));
    assert_eq!(list[1]["title"], json!("第二章 新乙"));

    // 书 B：applyTitles=false 时不写覆盖
    let book_b = server
        .upload_txt("导入改名B", "第一章 甲\n正文甲。\n")
        .await;
    let book_id_b = book_b["bookId"].as_str().unwrap().to_string();
    let result = server
        .import_multipart(
            &[
                ("bookId", book_id_b.as_str()),
                ("mode", "book"),
                ("applyTitles", "false"),
            ],
            "第一章 不改\n导入正文。\n",
        )
        .await;
    assert_eq!(result["titled"], json!(0));
    let list = server.chapter_list(&book_id_b).await;
    assert_eq!(list[0]["title"], json!("第一章 甲"));
}

#[tokio::test]
async fn cached_search_uses_overridden_title() {
    let server = TestServer::start().await;
    let book_id = server.register_placeholder_book("检索改名测试").await;
    server
        .seed_toc(&book_id, &[("第一章 开始", "http://up/ch/1")])
        .await;
    server
        .seed_chapter_cache(&book_id, "http://up/ch/1", "他走进了那座古庙。")
        .await;

    server
        .post(
            "renameChapter",
            json!({ "bookId": book_id, "chapterUrl": "http://up/ch/1", "title": "第一章 古庙" }),
        )
        .await;

    let (_, payload) = server
        .post_raw(
            "searchCachedContent",
            json!({ "bookId": book_id, "query": "古庙" }),
        )
        .await;
    assert_eq!(payload["isSuccess"], json!(true));
    let results = payload["data"]["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["chapterTitle"],
        json!("第一章 古庙"),
        "命中条目标题应与目录显示一致"
    );
}
