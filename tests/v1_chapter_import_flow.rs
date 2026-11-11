//! 整本 TXT 导入链（v1）：分片上传、预览、批量导入、逐章差异。
//! 本地 TXT 作夹具（目录来自文件），走 v1 路径身份。

mod common;
use axum::http::StatusCode;
use serde_json::{json, Value};
use std::path::PathBuf;

use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
    book_id: String,
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "v1-import-flow-secret".to_string(),
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
            .post(format!("{base_url}/v1/auth/login"))
            .header("x-requested-with", "XMLHttpRequest")
            .json(&json!({ "username": "reader1", "password": "password123" }))
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        let token = payload["accessToken"].as_str().unwrap().to_string();

        // 本地 TXT 夹具：三章
        let boundary = "----v1-import-test";
        let txt =
            "第一章 开始\n原始正文一。\n第二章 继续\n原始正文二。\n第三章 尾声\n原始正文三。\n";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"test.txt\"\r\nContent-Type: text/plain\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(txt.as_bytes());
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let resp = client
            .post(format!("{base_url}/reader3/uploadTxtBook"))
            .bearer_auth(&token)
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
        let book_id = payload["data"]["bookId"].as_str().unwrap().to_string();

        Self {
            base_url,
            temp_dir,
            client,
            token,
            book_id,
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    /// 手拼 multipart 打 v1 导入端点；`file` 段放在最后（与前端一致）。
    async fn post_multipart(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        file: Option<&str>,
    ) -> (StatusCode, Value) {
        let boundary = "----v1-import-test";
        let mut body = String::new();
        for (name, value) in fields {
            body.push_str(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            ));
        }
        if let Some(content) = file {
            body.push_str(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"import.txt\"\r\nContent-Type: text/plain\r\n\r\n{content}\r\n"
            ));
        }
        body.push_str(&format!("--{boundary}--\r\n"));
        let resp = self
            .client
            .post(format!("{}{path}", self.base_url))
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
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

const IMPORT_TXT: &str =
    "第一章 开始\n导入正文一。\n第二章 继续\n导入正文二。\n第三十七章 不存在\n这段不应该匹配。\n";

#[tokio::test]
async fn preview_import_and_diff_flow() {
    let server = TestServer::start().await;
    let preview_path = format!("/v1/books/{}/chapters:preview-import", server.book_id);
    let import_path = format!("/v1/books/{}/chapters:import", server.book_id);

    // 预览：两章匹配 + 一个目录里不存在的「第三十七章」
    let (status, preview) = server
        .post_multipart(
            &preview_path,
            &[("bookId", &server.book_id), ("mode", "book")],
            Some(IMPORT_TXT),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["mode"], json!("book"));
    assert_eq!(preview["total"], json!(3));
    assert_eq!(preview["matched"], json!(2));
    assert_eq!(preview["unmatched"], json!(1));
    let preview_id = preview["previewId"].as_str().unwrap().to_string();

    // 逐章差异：第 0 章与当前正文不同。
    // 本地书的正文不入正文池，diff 会在「本章还没有缓存正文」上短路——
    // 改用网络书走同一路径验证（见 network_book_import_diff）。
    // 这里只验证预览与导入主链路。

    // 导入：只选前两章（第三十七章不选）
    let (status, result) = server
        .post_multipart(
            &import_path,
            &[
                ("bookId", &server.book_id),
                ("mode", "book"),
                ("kind", "imported"),
                ("note", "整本导入测试"),
                ("makeCurrent", "true"),
                ("previewId", &preview_id),
                ("selectedIndexes", "0,1"),
            ],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["imported"], json!(2), "{result}");
    assert_eq!(result["failed"], json!(0));
    assert_eq!(result["mode"], json!("book"));

    // 导入后的正文可从 v1 内容端点读到
    let (status, content) = server
        .get(&format!("/v1/books/{}/chapters/0/content", server.book_id))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content["body"], json!("导入正文一。"));
    assert_eq!(content["edited"], json!(true));

    server.cleanup();
}

#[tokio::test]
async fn chunked_upload_feeds_preview() {
    let server = TestServer::start().await;
    let chunk_path = format!("/v1/books/{}/chapters:upload-import-chunk", server.book_id);

    // 两片上传同一本 TXT（自适应分片的客户端把 file 切开）
    let full = IMPORT_TXT;
    let split_at = full.find("第二章 继续").unwrap();
    let (first, second) = full.split_at(split_at);

    // 手拼分片 multipart：uploadId 第一片缺省，第二片续传。
    // 分段字面量避免转义混乱；`CRLF` 是 `\r\n`。
    const CRLF: &str = "\r\n";
    const B: &str = "----v1-import-test";
    // 每个 multipart part 之间都要有 `--{boundary}\r\n` 分隔线
    let field_sep = format!("--{B}{CRLF}");
    let chunk_body = |upload_id: Option<&str>, offset: usize| {
        let mut body = String::new();
        body.push_str(&field_sep);
        if let Some(id) = upload_id {
            body.push_str("Content-Disposition: form-data; name=\"uploadId\"");
            body.push_str(CRLF);
            body.push_str(CRLF);
            body.push_str(id);
            body.push_str(CRLF);
            body.push_str(&field_sep);
        }
        body.push_str("Content-Disposition: form-data; name=\"offset\"");
        body.push_str(CRLF);
        body.push_str(CRLF);
        body.push_str(&offset.to_string());
        body.push_str(CRLF);
        body.push_str(&field_sep);
        body.push_str("Content-Disposition: form-data; name=\"fileSize\"");
        body.push_str(CRLF);
        body.push_str(CRLF);
        body.push_str(&full.len().to_string());
        body.push_str(CRLF);
        body.push_str(&field_sep);
        let (content_start, content_end) = if offset == 0 {
            (0, first.len())
        } else {
            (first.len(), full.len())
        };
        body.push_str("Content-Disposition: form-data; name=\"chunk\"; filename=\"chunk.bin\"");
        body.push_str(CRLF);
        body.push_str("Content-Type: application/octet-stream");
        body.push_str(CRLF);
        body.push_str(CRLF);
        body.push_str(&full[content_start..content_end]);
        body.push_str(CRLF);
        body.push_str("--");
        body.push_str(B);
        body.push_str("--");
        body.push_str(CRLF);
        body
    };

    // 第一片：无 uploadId，服务端分配
    let resp = server
        .client
        .post(format!("{}{chunk_path}", server.base_url))
        .bearer_auth(&server.token)
        .header(
            "Content-Type",
            "multipart/form-data; boundary=----v1-import-test",
        )
        .body({
            let b = chunk_body(None, 0);
            println!("CHUNK BODY LEN={}", b.len());
            println!("CHUNK BODY: {:?}", b);
            b
        })
        .send()
        .await
        .unwrap();
    let body_text = resp.text().await.unwrap_or_default();
    println!("CHUNK RESP: {body_text}");
    let payload: Value = serde_json::from_str(&body_text)
        .unwrap_or_else(|e| panic!("chunk 1 响应解析失败: {e} — {body_text}"));
    let upload_id = payload["uploadId"].as_str().unwrap_or_default().to_string();
    assert!(!upload_id.is_empty(), "chunk 1: {body_text}");
    assert_eq!(
        payload["received"],
        json!(36),
        "第一片 36 字节: {body_text}"
    );
    assert_eq!(payload["complete"], json!(false));

    // 第二片：带 uploadId 续传
    let resp = server
        .client
        .post(format!("{}{chunk_path}", server.base_url))
        .bearer_auth(&server.token)
        .header(
            "Content-Type",
            "multipart/form-data; boundary=----v1-import-test",
        )
        .body(chunk_body(Some(&upload_id), first.len()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let payload: Value = resp.json().await.unwrap();
    assert_eq!(
        payload["complete"],
        json!(true),
        "两片拼完应标记完成: {payload}"
    );

    // 预览凭 uploadId 复用拼好的文件，不再上传正文
    let (status, preview) = server
        .post_multipart(
            &format!("/v1/books/{}/chapters:preview-import", server.book_id),
            &[
                ("bookId", &server.book_id),
                ("mode", "book"),
                ("uploadId", &upload_id),
            ],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        preview["total"],
        json!(3),
        "分片拼出的整本应有三章: {preview}"
    );
    assert_eq!(preview["matched"], json!(2));

    server.cleanup();
}

#[tokio::test]
async fn import_requires_auth_and_book_match() {
    let server = TestServer::start().await;

    // 未登录：401 problem（带 CSRF 头绕过 CSRF 层，让鉴权层可见）
    let resp = server
        .client
        .post(format!(
            "{}/v1/books/{}/chapters:preview-import",
            server.base_url, server.book_id
        ))
        .header("x-requested-with", "XMLHttpRequest")
        .header("Content-Type", "multipart/form-data; boundary=x")
        .body("--x--\r\n")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");

    // 表单 bookId 与路径不一致：以路径为准，无跨书问题
    let (status, _) = server
        .post_multipart(
            &format!("/v1/books/{}/chapters:preview-import", server.book_id),
            &[("bookId", "book_999999"), ("mode", "book")],
            Some(IMPORT_TXT),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "路径 bookId 覆盖表单值");

    server.cleanup();
}
