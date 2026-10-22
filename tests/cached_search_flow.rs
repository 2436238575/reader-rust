//! 缓存内全文检索的端到端行为：只扫已缓存章节正文（可再生数据），
//! 命中返回章节定位 + 上下文摘要；不触发任何上游请求。

use axum::http::StatusCode;
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use reader_rust::util::hash::md5_hex;
use serde_json::{json, Value};
use std::path::PathBuf;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

impl TestServer {
    async fn start() -> Self {
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

    /// 预置章节正文缓存：`<storage>/cache/user_1/<bookId>/<md5前2位>/<md5>.txt`
    fn seed_chapter_cache(&self, book_id: &str, chapter_url: &str, content: &str) {
        let name = md5_hex(chapter_url);
        let dir = self
            .temp_dir
            .join("cache")
            .join("user_1")
            .join(book_id)
            .join(&name[..2]);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.txt")), content).unwrap();
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
        let _ = std::fs::remove_dir_all(&self.temp_dir);
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
    server.seed_chapter_cache(&book_id, "http://up/ch/1", "他走进了那座古庙。");
    server.seed_chapter_cache(
        "book_1",
        "http://up/ch/2",
        "离开之后她又回到古庙前，天色已经完全暗了下来，只有庙里的长明灯还亮着。她站在石阶下仰头望去，飞檐的轮廓在夜色里沉默得像一头伏着的兽，很久都没有动。",
    );
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
    server.seed_chapter_cache(
        "book_1",
        "http://up/ch/1",
        "<p>He walked <b>Silent</b>ly.</p><p>下一行 silent 再来一次。</p>",
    );

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
        server.seed_chapter_cache(&book_id, url, "每一章都提到那个人。");
    }

    let (status, payload) = server
        .search(json!({ "bookId": book_id, "query": "那个人", "maxResults": 1 }))
        .await;

    assert_eq!(status, StatusCode::OK);
    let data = &payload["data"];
    assert_eq!(data["results"].as_array().unwrap().len(), 1);
    assert_eq!(data["truncated"], json!(true));
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
