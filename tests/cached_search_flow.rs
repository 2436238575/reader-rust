//! 缓存内全文检索的端到端行为：只扫已缓存章节正文（可再生数据），
//! 命中返回章节定位 + 上下文摘要；不触发任何上游请求。

mod common;
use axum::http::StatusCode;
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
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-search-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "search-test-secret".to_string(),
            request_timeout_secs: 10,
            admin_username: "reader1".to_string(),
            admin_password: "password123".to_string(),
            rate_limit_disabled: true,
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
            .post(format!("{base_url}/reader3/login"))
            .json(&json!({ "username": "reader1", "password": "password123" }))
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        let token = payload["data"]["accessToken"].as_str().unwrap().to_string();
        Self {
            base_url,
            temp_dir,
            client,
            token,
            content,
        }
    }

    /// 预置目录缓存：`<storage>/cache/user_1/toc/<bookId>.json`
    fn seed_toc(&self, book_id: &str, chapters: &[(&str, &str)]) {
        let dir = self.temp_dir.join("cache").join("user_1").join("toc");
        std::fs::create_dir_all(&dir).unwrap();
        let list: Vec<Value> = chapters
            .iter()
            .enumerate()
            .map(|(index, (title, url))| {
                json!({ "title": title, "url": url, "index": index as i64 })
            })
            .collect();
        std::fs::write(
            dir.join(format!("{book_id}.json")),
            serde_json::to_string(&list).unwrap(),
        )
        .unwrap();
    }

    /// 预置一组上游正文版本（正文池）。
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

    /// 通用 POST（成功断言 200 + isSuccess，返回 data）。
    async fn post(&self, path: &str, body: Value) -> Value {
        let resp = self
            .client
            .post(format!("{}/reader3/{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        assert_eq!(
            payload["isSuccess"],
            json!(true),
            "{path} 应成功: {payload}"
        );
        payload["data"].clone()
    }

    /// 登记一本书（search_cached_content 现在要求书已存在——resolve_book 校验）。
    /// 用本地 TXT 上传登记最省事（不需要书源）。
    async fn register_book(&self, name: &str, content: &str) -> String {
        let boundary = "----cached-search-test";
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
        payload["data"]["bookId"].as_str().unwrap().to_string()
    }

    async fn search(&self, body: Value) -> (StatusCode, Value) {
        let resp = self
            .client
            .post(format!("{}/reader3/searchCachedContent", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        (status, resp.json().await.unwrap())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

#[tokio::test]
async fn search_finds_across_cached_chapters_with_coverage() {
    let server = TestServer::start().await;
    let book_id = server.register_book("检索测试", "占位").await;
    let chapters = [
        ("第一章 开始", "http://up/ch/1"),
        ("第二章 发展", "http://up/ch/2"),
        ("第三章 高潮", "http://up/ch/3"),
    ];
    server.seed_toc(&book_id, &chapters);
    server
        .seed_chapter_cache(&book_id, "http://up/ch/1", "他走进了那座古庙。")
        .await;
    server
        .seed_chapter_cache(
            &book_id,
            "http://up/ch/2",
            "离开之后她又回到古庙前，天色已经完全暗了下来，只有庙里的长明灯还亮着。她站在石阶下仰头望去，飞檐的轮廓在夜色里沉默得像一头伏着的兽，很久都没有动。",
        )
        .await;
    // 第三章故意不写缓存文件：未缓存章节不参与检索，只体现在覆盖度里

    let (status, payload) = server
        .search(json!({ "bookId": book_id, "query": "古庙" }))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["isSuccess"], json!(true));
    let data = &payload["data"];
    let results = data["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["chapterIndex"], json!(0));
    assert_eq!(results[0]["chapterTitle"], json!("第一章 开始"));
    // 命中来自上游缓存（非编辑版）：edited 恒 false
    assert_eq!(results[0]["edited"], json!(false));
    assert_eq!(results[1]["edited"], json!(false));
    let snippet = results[0]["snippets"][0].as_str().unwrap();
    assert!(snippet.contains("古庙"), "摘要应含命中词: {snippet}");
    // 第二章命中处前后都有长文：摘要两头带省略号
    let snippet2 = results[1]["snippets"][0].as_str().unwrap();
    assert!(snippet2.ends_with('…'), "截断的摘要应带省略号: {snippet2}");
    // 覆盖度：3 章可检索、2 章有缓存
    assert_eq!(data["totalChapters"], json!(3));
    assert_eq!(data["cachedChapters"], json!(2));
    assert_eq!(data["truncated"], json!(false));
}

#[tokio::test]
async fn search_is_case_insensitive_and_strips_html() {
    let server = TestServer::start().await;
    let book_id = server.register_book("检索测试", "占位").await;
    server.seed_toc(&book_id, &[("第一章", "http://up/ch/1")]);
    server
        .seed_chapter_cache(
            &book_id,
            "http://up/ch/1",
            "<p>He walked <b>Silent</b>ly.</p><p>下一行 silent 再来一次。</p>",
        )
        .await;

    let (status, payload) = server
        .search(json!({ "bookId": book_id, "query": "silent" }))
        .await;

    assert_eq!(status, StatusCode::OK);
    let data = &payload["data"];
    let snippets = data["results"][0]["snippets"].as_array().unwrap();
    assert_eq!(snippets.len(), 2, "同一章多处命中应各出一条摘要");
    assert!(
        snippets.iter().all(|s| !s.as_str().unwrap().contains('<')),
        "HTML 标签应剥掉: {snippets:?}"
    );
}

#[tokio::test]
async fn search_truncates_at_max_results() {
    let server = TestServer::start().await;
    let book_id = server.register_book("截断测试", "占位").await;
    let chapters: Vec<(String, String)> = (1..=3)
        .map(|i| (format!("第{i}章"), format!("http://up/ch/{i}")))
        .collect();
    let refs: Vec<(&str, &str)> = chapters
        .iter()
        .map(|(t, u)| (t.as_str(), u.as_str()))
        .collect();
    server.seed_toc(&book_id, &refs);
    for (_, url) in &refs {
        server
            .seed_chapter_cache(&book_id, url, "每一章都提到那个人。")
            .await;
    }

    let (status, payload) = server
        .search(json!({ "bookId": book_id, "query": "那个人", "maxResults": 1 }))
        .await;

    assert_eq!(status, StatusCode::OK);
    let data = &payload["data"];
    assert_eq!(data["results"].as_array().unwrap().len(), 1);
    assert_eq!(data["truncated"], json!(true));
}

/// 跨 blocking 批次（每批 100 章）的顺序与命中限额：250 章全命中，
/// maxResults=150 让限额落在第 2 批中段。
#[tokio::test]
async fn search_keeps_order_across_blocking_batches_and_limits_inside_later_batch() {
    let server = TestServer::start().await;
    let book_id = server.register_book("跨批顺序测试", "占位").await;

    let chapter_list: Vec<(String, String)> = (0..250)
        .map(|i| (format!("第{}章", i + 1), format!("http://up/batch/{i}")))
        .collect();
    let refs: Vec<(&str, &str)> = chapter_list
        .iter()
        .map(|(t, u)| (t.as_str(), u.as_str()))
        .collect();
    server.seed_toc(&book_id, &refs);
    for (i, (_, url)) in chapter_list.iter().enumerate() {
        server
            .seed_chapter_cache(&book_id, url, &format!("第{i}节的雪山与长风。"))
            .await;
    }

    let (status, payload) = server
        .search(json!({ "bookId": book_id, "query": "雪山", "maxResults": 150 }))
        .await;

    assert_eq!(status, StatusCode::OK);
    let data = &payload["data"];
    let results = data["results"].as_array().unwrap();
    assert_eq!(results.len(), 150, "命中限额在 150 生效");
    // 顺序必须严格按章递增（第 1 批 100 条 + 第 2 批 50 条，无跨批错乱）
    for (i, hit) in results.iter().enumerate() {
        assert_eq!(
            hit["chapterIndex"],
            json!(i),
            "第 {i} 条命中应来自第 {i} 章"
        );
    }
    // 限额落在第 149 章（第 2 批中段），后面还有 100 章 → 截断为真
    assert_eq!(data["truncated"], json!(true));
    // 参与检索的章节数：批粒度加载不虚报（150 章实际过了一遍）
    assert_eq!(data["cachedChapters"], json!(150));
    assert_eq!(data["totalChapters"], json!(250));
}

#[tokio::test]
async fn search_validates_input_and_handles_empty_cache() {
    let server = TestServer::start().await;
    let book_id = server.register_book("校验测试", "占位").await;

    // 空 query / 超长 query / 空 bookId 都是 400
    for body in [
        json!({ "bookId": &book_id, "query": "  " }),
        json!({ "bookId": &book_id, "query": "x".repeat(101) }),
        json!({ "bookId": "", "query": "词" }),
    ] {
        let (status, _) = server.search(body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // 已登记但没有任何缓存的书：成功返回空结果，覆盖度如实为 0
    let empty_book = server.register_book("空缓存书", "占位").await;
    let (status, payload) = server
        .search(json!({ "bookId": empty_book, "query": "词" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let data = &payload["data"];
    assert_eq!(data["results"], json!([]));
    assert_eq!(data["totalChapters"], json!(0));
    assert_eq!(data["cachedChapters"], json!(0));
}

#[tokio::test]
async fn search_reads_edited_version_when_present() {
    let server = TestServer::start().await;
    let book_id = server.register_book("编辑版检索", "占位").await;
    server.seed_toc(&book_id, &[("第一章", "http://up/ch/1")]);
    server
        .seed_chapter_cache(&book_id, "http://up/ch/1", "上游缓存里的旧词。")
        .await;
    // 编辑版把内容改了：检索应命中编辑版而不是上游缓存
    server
        .post(
            "saveChapterEdit",
            json!({
                "bookId": book_id,
                "chapterUrl": "http://up/ch/1",
                "chapterIndex": 0,
                "chapterTitle": "第一章",
                "content": "编辑版里的新词。",
            }),
        )
        .await;

    let (status, payload) = server
        .search(json!({ "bookId": book_id, "query": "新词" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let results = payload["data"]["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["edited"], json!(true), "命中编辑版应带标记");

    // 上游旧词搜不到（编辑版覆盖了它）
    let (_, payload) = server
        .search(json!({ "bookId": book_id, "query": "旧词" }))
        .await;
    assert_eq!(payload["data"]["results"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn search_requires_login() {
    let server = TestServer::start().await;
    let resp = server
        .client
        .post(format!("{}/reader3/searchCachedContent", server.base_url))
        .json(&json!({ "bookId": "book_1", "query": "词" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// deleteBookCache / purgeCache(scope=book) 的 bookId 与书籍级接口同口径：
/// 未登记或非法格式 404（到不了文件系统）；别名（book_01）归一到 canonical
/// 路径清理——同一本书不该有两个缓存文件。
#[tokio::test]
async fn book_cache_delete_validates_and_normalizes_book_id() {
    let server = TestServer::start().await;
    let book_id = server.register_book("缓存清理", "占位").await;
    server.seed_toc(&book_id, &[("第一章", "http://up/ch/1")]);
    server
        .seed_chapter_cache(&book_id, "http://up/ch/1", "正文。")
        .await;
    let toc_path = server
        .temp_dir
        .join("cache")
        .join("user_1")
        .join("toc")
        .join(format!("{book_id}.json"));
    assert!(toc_path.exists());

    // 别名 book_01（前导零）与 canonical 指向同一本书
    let alias = format!("book_0{}", book_id.trim_start_matches("book_"));
    let resp = server
        .client
        .post(format!("{}/reader3/purgeCache", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "scope": "book", "bookId": alias }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "别名应解析到同一本书");
    assert!(!toc_path.exists(), "canonical toc 缓存应被别名调用清掉");

    // 越界 id：两个端点都 404；toc 目录上一级的文件必须原样保留
    let keep = server
        .temp_dir
        .join("cache")
        .join("user_1")
        .join("keep.json");
    std::fs::write(&keep, b"{}").unwrap();
    for (path, body) in [
        ("/reader3/deleteBookCache", json!({ "bookId": "../keep" })),
        (
            "/reader3/purgeCache",
            json!({ "scope": "book", "bookId": "../keep" }),
        ),
    ] {
        let resp = server
            .client
            .post(format!("{}{path}", server.base_url))
            .bearer_auth(&server.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "{path} 越界 id 应 404"
        );
    }
    assert!(keep.exists(), "目录外的文件绝不能被动到");

    // 未登记的合法 id 同样 404（存在性校验）
    let resp = server
        .client
        .post(format!("{}/reader3/deleteBookCache", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "bookId": "book_999" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
