//! `/v1/sources/{id}/explore` 与 `POST /v1/local-books`：非 P2 依赖的同步端点。

mod common;
use axum::http::StatusCode;
use serde_json::{json, Value};
use std::path::PathBuf;

use axum::Json;
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
    source_id: String,
}

async fn start_explore_server() -> (String, String) {
    // 假上游：探索分类 + 书单（封面走图片管道收口）
    let app = axum::Router::new().route(
        "/discover",
        axum::routing::get(|| async {
            Json(json!({
                "data": { "list": [
                    {
                        "name": "探索书一",
                        "author": "作者甲",
                        "bookUrl": "http://up.example/book/1",
                        "coverUrl": "/cover/1.jpeg",
                        "intro": "简介一",
                        "kind": "玄幻"
                    },
                    {
                        "name": "探索书二",
                        "author": "作者乙",
                        "bookUrl": "http://up.example/book/2"
                    }
                ]}
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), "/cover/1.jpeg".to_string())
}

async fn boot(upstream_url: &str) -> TestServer {
    common::clean_stale_temp_dirs();
    let temp_dir =
        std::env::temp_dir().join(format!("reader-rust-v1-nonp2-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let cfg = AppConfig {
        server_host: "127.0.0.1".to_string(),
        server_port: 0,
        database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
        storage_dir: temp_dir.to_string_lossy().to_string(),
        web_root: temp_dir.to_string_lossy().to_string(),
        assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
        jwt_secret: "v1-nonp2-secret".to_string(),
        request_timeout_secs: 10,
        admin_username: "reader1".to_string(),
        admin_password: "password123".to_string(),
        rate_limit_disabled: true,
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

    // 假书源：exploreUrl 指向假上游的分类地址
    let resp = client
        .post(format!("{base_url}/v1/sources"))
        .bearer_auth(&token)
        .json(&json!({
            "bookSourceName": "假源",
            "bookSourceUrl": upstream_url,
            "enabled": true,
            "ruleExplore": {
                "bookList": "$.data.list[*]",
                "name": "$.name",
                "author": "$.author",
                "bookUrl": "$.bookUrl",
                "coverUrl": "$.coverUrl",
                "intro": "$.intro",
                "kind": "$.kind"
            },
            "exploreUrl": format!("{upstream_url}/discover")
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let payload: Value = resp.json().await.unwrap();
    let source_id = payload["id"].as_str().unwrap().to_string();

    TestServer {
        base_url,
        temp_dir,
        client,
        token,
        source_id,
    }
}

impl TestServer {
    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let resp = self
            .client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let payload: Value = resp.json().await.unwrap();
        (status, payload)
    }

    /// 手拼 multipart 上传本地书到 v1。
    async fn upload_local(&self, filename: &str, content: &[u8]) -> (StatusCode, Value) {
        let boundary = "----v1-nonp2-test";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: text/plain\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(content);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let resp = self
            .client
            .post(format!("{}/v1/local-books", self.base_url))
            .bearer_auth(&self.token)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let payload: Value = resp.json().await.unwrap();
        (status, payload)
    }

    /// 带 `?kind=` 查询参数的上传（显式格式覆盖分支）。
    async fn upload_local_with_kind(
        &self,
        filename: &str,
        content: &[u8],
        kind: Option<&str>,
    ) -> (StatusCode, Value) {
        let boundary = "----v1-nonp2-test";
        let kind_param = kind.unwrap_or_default();
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: text/plain\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(content);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let mut url = format!("{}/v1/local-books", self.base_url);
        if !kind_param.is_empty() {
            url.push_str(&format!("?kind={kind_param}"));
        }
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let payload: Value = resp.json().await.unwrap();
        (status, payload)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

#[tokio::test]
async fn explore_returns_items_with_pipeline_covers() {
    let (upstream_url, _) = start_explore_server().await;
    let server = boot(&upstream_url).await;
    let path = format!("/v1/sources/{}/explore", server.source_id);

    let (status, body) = server
        .get(&format!("{path}?url={}/discover&page=1", upstream_url))
        .await;
    assert_eq!(status, StatusCode::OK);
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["name"], json!("探索书一"));
    assert_eq!(items[0]["bookId"], json!("book_1"), "搜索/探索产出即登记");
    // 封面收口：有封面的条目换成 /reader3/image/<id>，没有的原样缺失
    let cover = items[0]["coverUrl"].as_str().unwrap();
    assert!(cover.starts_with("/reader3/image/"), "封面收口: {cover}");
    assert!(items[1]["coverUrl"].is_null());

    server.cleanup();
}

#[tokio::test]
async fn explore_validates_params_and_unknown_source() {
    let (upstream_url, _) = start_explore_server().await;
    let server = boot(&upstream_url).await;

    // url 缺失：400
    let (status, body) = server
        .get(&format!("/v1/sources/{}/explore", server.source_id))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], json!("MALFORMED_REQUEST"));

    // 未知书源：404 精确码
    let (status, body) = server
        .get(&format!(
            "/v1/sources/src_999999/explore?url={}/discover",
            upstream_url
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], json!("SOURCE_NOT_FOUND"));

    server.cleanup();
}

#[tokio::test]
async fn local_book_upload_creates_book() {
    let server = boot("http://unused.example").await;

    let (status, body) = server
        .upload_local(
            "测试书.txt",
            "第一章 开始\n上传正文一。\n第二章 继续\n上传正文二。\n".as_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let book_id = body["bookId"].as_str().unwrap().to_string();
    assert!(book_id.starts_with("book_"));

    // 本地书内容可从 v1 读到
    let (status, content) = server
        .get(&format!("/v1/books/{book_id}/chapters/0/content"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content["body"], json!("上传正文一。"));

    // EPUB 扩展名分派（无效 EPUB 内容 → 400 解析失败）
    let (epub_status, _body) = server.upload_local("测试书.epub", b"not an epub").await;
    assert_eq!(epub_status, StatusCode::BAD_REQUEST);

    // file 缺失：400
    let boundary = "----v1-nonp2-test";
    let resp = server
        .client
        .post(format!("{}/v1/local-books", server.base_url))
        .bearer_auth(&server.token)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(format!("--{boundary}--\r\n"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // ?kind=epub + 51MB 假 body：走 EPUB 的 80MB 上限，应该报「解析失败」
    // 而不是 413 体积超限（区分 TXT 50MB 与 EPUB 80MB 两条分支）
    let big_content = vec![0x4eu8; 51 * 1024 * 1024]; // 51MB of 'N'
    let boundary = "----v1-nonp2-test";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.epub\"\r\nContent-Type: application/epub+zip\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(&big_content);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let resp = server
        .client
        .post(format!("{}/v1/local-books?kind=epub", server.base_url))
        .bearer_auth(&server.token)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["detail"],
        json!("EPUB 文件损坏"),
        "应走 EPUB 80MB 读体上限后在解析阶段失败，而不是被 TXT 50MB 读体上限 413"
    );

    server.cleanup();
}

/// 上传路由的自挂中间件（CSRF/鉴权）与 `?kind=` 显式覆盖。
#[tokio::test]
async fn local_books_middleware_and_kind_override() {
    let server = boot("http://unused.example").await;

    // 无 Bearer 也无 CSRF 头 → 403（CSRF 层先于鉴权层可见）
    let resp = server
        .client
        .post(format!("{}/v1/local-books", server.base_url))
        .header("Content-Type", "multipart/form-data; boundary=x")
        .body("--x--\r\n")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 有 CSRF 头无 Bearer → 401（鉴权层）
    let resp = server
        .client
        .post(format!("{}/v1/local-books", server.base_url))
        .header("x-requested-with", "XMLHttpRequest")
        .header("Content-Type", "multipart/form-data; boundary=x")
        .body("--x--\r\n")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // kind=epub 显式覆盖 .txt 文件名：坏 EPUB 内容 → 400（分派到了 EPUB）
    let (status, _) = server
        .upload_local_with_kind("测试书.txt", b"not an epub", Some("epub"))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "kind=epub 应分派到 EPUB");

    // kind=txt 显式覆盖 .epub 文件名：分派到 TXT 后由 TXT 扩展名校验拒绝
    // （底层服务仍校验自己的扩展名——kind 覆盖的是路由层分派，不越权改服务）
    let (status, body) = server
        .upload_local_with_kind(
            "测试书.epub",
            "第一章 开始\n上传正文。\n".as_bytes(),
            Some("txt"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], json!("仅支持上传 .txt 文件"));

    // kind=garbage：显式 400，不再静默按扩展名回落
    let (status, body) = server
        .upload_local_with_kind("测试书.txt", "第一章\n正文".as_bytes(), Some("garbage"))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["detail"],
        json!("kind 只接受 txt 或 epub，收到 garbage"),
        "非法 kind 应显式 400: {body}"
    );

    server.cleanup();
}

/// 本地书数量上限：第 N+1 本 → 400，且已落盘文件被清理（不留半本书）。
#[tokio::test]
async fn local_books_enforce_count_limit() {
    // 上限 1 本：第二本 → 400
    common::clean_stale_temp_dirs();
    let temp_dir = std::env::temp_dir().join(format!(
        "reader-rust-v1-nonp2-limit-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let cfg = AppConfig {
        server_host: "127.0.0.1".to_string(),
        server_port: 0,
        database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
        storage_dir: temp_dir.to_string_lossy().to_string(),
        web_root: temp_dir.to_string_lossy().to_string(),
        assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
        jwt_secret: "v1-nonp2-limit-secret".to_string(),
        request_timeout_secs: 10,
        admin_username: "reader1".to_string(),
        admin_password: "password123".to_string(),
        rate_limit_disabled: true,
        allow_private_network: true,
        user_local_book_limit: 1,
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
        .post(format!("{base_url}/v1/auth/login"))
        .header("x-requested-with", "XMLHttpRequest")
        .json(&json!({ "username": "reader1", "password": "password123" }))
        .send()
        .await
        .unwrap();
    let token = resp.json::<Value>().await.unwrap()["accessToken"]
        .as_str()
        .unwrap()
        .to_string();

    let upload = |name: &str| {
        let boundary = "----v1-nonp2-test";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: text/plain\r\n\r\n第一章 开始\n正文。\r\n--{boundary}--\r\n"
        )
        .into_bytes();
        let _ = &mut body;
        client
            .post(format!("{base_url}/v1/local-books"))
            .bearer_auth(&token)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
    };
    let _ = &upload;

    let resp = client
        .post(format!("{base_url}/v1/local-books"))
        .bearer_auth(&token)
        .header(
            "Content-Type",
            "multipart/form-data; boundary=----v1-nonp2-test",
        )
        .body(
            "------v1-nonp2-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\n第一章 开始\n正文一。\r\n------v1-nonp2-test--\r\n",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = client
        .post(format!("{base_url}/v1/local-books"))
        .bearer_auth(&token)
        .header(
            "Content-Type",
            "multipart/form-data; boundary=----v1-nonp2-test",
        )
        .body(
            "------v1-nonp2-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"b.txt\"\r\nContent-Type: text/plain\r\n\r\n第一章 开始\n正文二。\r\n------v1-nonp2-test--\r\n",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超限应 400");

    // 超限被拒的书的落盘文件已被清理：data/user_1/local_books 下只有第一本的目录
    let local_dir = temp_dir.join("data").join("user_1").join("local_books");
    let entries: Vec<String> = std::fs::read_dir(&local_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries.len(), 1, "超限书的落盘文件应被清理: {entries:?}");

    let _ = std::fs::remove_dir_all(&temp_dir);
}
