//! 章节正文编辑版的端到端行为：
//! - 编辑版优先于一切缓存与上游（本地书原文切片 / 网络书 FileCache）；
//! - `refresh=1` 重抓上游更新缓存基准，但返回值仍是编辑版；
//! - `variant=upstream` 只读基准（diff 通道）；删除编辑版即恢复上游版本；
//! - 删书级联清掉编辑版目录。

use axum::{http::StatusCode, routing::get, Json, Router};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

async fn boot() -> TestServer {
    let temp_dir =
        std::env::temp_dir().join(format!("reader-rust-edit-flow-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let cfg = AppConfig {
        server_host: "127.0.0.1".to_string(),
        server_port: 0,
        database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
        storage_dir: temp_dir.to_string_lossy().to_string(),
        web_root: temp_dir.to_string_lossy().to_string(),
        assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
        jwt_secret: "edit-flow-secret".to_string(),
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
    let resp = client
        .post(format!("{base_url}/reader3/login"))
        .json(&json!({ "username": "reader1", "password": "password123" }))
        .send()
        .await
        .unwrap();
    let payload: Value = resp.json().await.unwrap();
    let token = payload["data"]["accessToken"].as_str().unwrap().to_string();
    TestServer {
        base_url,
        temp_dir,
        client,
        token,
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

impl TestServer {
    async fn post(&self, path: &str, body: Value) -> Value {
        let resp = self
            .client
            .post(format!("{}/reader3/{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path} 应返回 200");
        let payload: Value = resp.json().await.unwrap();
        assert_eq!(
            payload["isSuccess"],
            json!(true),
            "{path} 应成功: {payload}"
        );
        payload["data"].clone()
    }

    async fn upload_txt(&self, name: &str, content: &str) -> Value {
        // reqwest 主 crate 没开 multipart feature：手工拼 multipart body
        let boundary = "----chapter-edit-test";
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

    async fn get_content(&self, book_id: &str, chapter_url: &str, extra: Value) -> String {
        let mut body = json!({ "bookId": book_id, "chapterUrl": chapter_url, "index": 0 });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        self.post("getBookContent", body)
            .await
            .as_str()
            .unwrap()
            .to_string()
    }
}

#[tokio::test]
async fn local_book_edit_overlays_original_and_delete_restores() {
    let server = boot().await;
    let book = server
        .upload_txt(
            "编辑流测试",
            "第一章 开始\n原始正文第一句。\n第二章 继续\n第二章内容。\n",
        )
        .await;
    let book_id = book["bookId"].as_str().unwrap();

    // 本地书的章节 URL 形如 local-txt:<hash>（index 由参数带）
    let chapters = server
        .post("getChapterList", json!({ "bookId": book_id }))
        .await;
    let chapter_url = chapters[0]["url"].as_str().unwrap().to_string();

    // 未编辑前：原文
    let before = server.get_content(book_id, &chapter_url, json!({})).await;
    assert!(before.contains("原始正文第一句"), "实际: {before}");

    // 保存编辑版
    server
        .post(
            "saveChapterEdit",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "chapterIndex": 0,
                "chapterTitle": "第一章 开始",
                "content": "编辑后的整章正文。",
            }),
        )
        .await;

    // 阅读链路返回编辑版；基准通道返回原文；清单能列出
    let after = server.get_content(book_id, &chapter_url, json!({})).await;
    assert_eq!(after, "编辑后的整章正文。");
    let upstream = server
        .get_content(book_id, &chapter_url, json!({ "variant": "upstream" }))
        .await;
    assert!(
        upstream.contains("原始正文第一句"),
        "基准应是原文: {upstream}"
    );
    let list = server
        .post("listChapterEdits", json!({ "bookId": book_id }))
        .await;
    assert_eq!(list["urls"].as_array().unwrap().len(), 1);

    // 删除编辑版 → 恢复原文
    server
        .post(
            "deleteChapterEdit",
            json!({ "bookId": book_id, "chapterUrl": chapter_url }),
        )
        .await;
    let restored = server.get_content(book_id, &chapter_url, json!({})).await;
    assert!(restored.contains("原始正文第一句"));

    // 再编辑后删书：编辑版目录被级联清掉
    server
        .post(
            "saveChapterEdit",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "chapterIndex": 0,
                "chapterTitle": "第一章 开始",
                "content": "再来一版",
            }),
        )
        .await;
    server
        .post("deleteBook", json!({ "bookId": book_id }))
        .await;
    let edits_dir = server
        .temp_dir
        .join("data")
        .join("user_1")
        .join("edited_chapters")
        .join(book_id);
    assert!(!edits_dir.exists(), "删书应级联清掉编辑版目录");
}

/// 网络书：refresh=1 重抓上游更新基准，阅读仍返回编辑版。
#[tokio::test]
async fn network_book_refresh_updates_base_but_returns_edit() {
    // 假上游：正文内容可切换版本
    let version = Arc::new(Mutex::new("上游第一版正文"));
    let upstream = {
        let version = version.clone();
        let app =
            Router::new().route(
                "/content",
                get(move || {
                    let version = version.clone();
                    async move {
                        Json(json!({ "data": { "content": version.lock().unwrap().clone() } }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    };

    let server = boot().await;
    // 书源 + 登记书
    server
        .post(
            "saveBookSource",
            json!({
                "bookSourceName": "假源",
                "bookSourceUrl": upstream,
                "enabled": true,
                "ruleContent": { "content": "$.data.content" }
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
                "bookUrl": format!("{upstream}/info"),
                "bookSourceId": source_id,
            }),
        )
        .await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let chapter_url = format!("{upstream}/content");

    // 首抓：上游第一版进缓存
    let first = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert_eq!(first, "上游第一版正文");

    // 保存编辑版后：阅读返回编辑版
    server
        .post(
            "saveChapterEdit",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "chapterIndex": 0,
                "chapterTitle": "第一章",
                "content": "我的编辑版",
            }),
        )
        .await;
    let edited = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert_eq!(edited, "我的编辑版");

    // 上游出第二版 + refresh=1：基准更新到第二版，阅读仍是编辑版
    *version.lock().unwrap() = "上游第二版正文";
    let after_refresh = server
        .get_content(&book_id, &chapter_url, json!({ "refresh": 1 }))
        .await;
    assert_eq!(after_refresh, "我的编辑版");
    let upstream_now = server
        .get_content(&book_id, &chapter_url, json!({ "variant": "upstream" }))
        .await;
    assert_eq!(upstream_now, "上游第二版正文", "基准应已更新到第二版");
}

/// diff 端点：系统有 git 时返回 ops；没有 git 的环境回退标记（跳过断言）。
#[tokio::test]
async fn diff_endpoint_runs_git_and_returns_ops() {
    let server = boot().await;
    let payload = server
        .post(
            "diffChapterEdit",
            json!({
                "base": ["一", "二", "三"],
                "edited": ["一", "二改", "三", "四"],
            }),
        )
        .await;
    if payload["fallback"].as_bool() == Some(true) {
        return; // 无 git 环境：回退路径已在 service 内联测试覆盖
    }
    let ops = payload["ops"].as_array().unwrap();
    let kinds: Vec<&str> = ops.iter().map(|op| op["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["equal", "modify", "equal", "add"]);
    assert_eq!(ops[1]["baseText"], json!("二"));
    assert_eq!(ops[1]["editedText"], json!("二改"));
    assert_eq!(ops[3]["editedIndex"], json!(3));
}
