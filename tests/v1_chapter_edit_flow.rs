//! 章节编辑域（v1）：编辑保存/取消、版本时间线、单版本、diff、版本选择、
//! 章节改名（覆盖层）、整书回上游。本地 TXT 作夹具（index 定位无需上游）。

mod common;
use axum::http::{header, StatusCode};
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
    /// 章节内部地址（v1 章节列表的过渡期只读 url 字段），chapter-marks 断言用
    chapter_url: String,
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-edit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "v1-edit-flow-secret".to_string(),
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

        // 本地 TXT 夹具：三章，index 定位无需上游
        let boundary = "----v1-edit-test";
        let txt =
            "第一章 开始\n原始正文第一句。\n第二章 继续\n第二章内容。\n第三章 尾声\n第三章内容。\n";
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

        // 章节内部地址（过渡期只读 url 字段）：chapter-marks 的键断言用
        let resp = client
            .get(format!("{base_url}/v1/books/{book_id}/chapters?limit=10"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        let chapter_url = payload["items"][1]["url"].as_str().unwrap().to_string();

        Self {
            base_url,
            temp_dir,
            client,
            token,
            book_id,
            chapter_url,
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    async fn put(&self, path: &str, body: Value) -> reqwest::Response {
        self.client
            .put(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn patch(&self, path: &str, body: Value) -> reqwest::Response {
        self.client
            .patch(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn post(&self, path: &str, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap()
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
async fn edit_save_cancel_and_version_flow() {
    let server = TestServer::start().await;
    let base = format!("/v1/books/{}/chapters/1", server.book_id);

    // 编辑保存：200 + cv id
    let resp = server
        .put(
            &format!("{base}/content"),
            json!({ "content": "编辑后的第二章。\n第二行。", "note": "手滑修正" }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["edited"], json!(true));
    let edited_id = body["versionId"].as_str().unwrap().to_string();
    assert!(edited_id.starts_with("cv_"));

    // 编辑器预填：当前编辑版
    let resp = server.get(&format!("{base}/content/edit")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["versionId"], json!(edited_id));
    assert_eq!(body["kind"], json!("edited"));

    // 没编辑的章：404 CHAPTER_EDIT_NOT_FOUND
    let resp = server
        .get(&format!(
            "/v1/books/{}/chapters/0/content/edit",
            server.book_id
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("CHAPTER_EDIT_NOT_FOUND"));

    // 导入第二份（不设当前），再选回编辑版
    let resp = server
        .post(
            &format!("{base}/content:import"),
            json!({ "content": "导入的历史版本。", "makeCurrent": false }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["becameCurrent"], json!(false));
    let imported_id = body["versionId"].as_str().unwrap().to_string();

    // 时间线：两条版本，cv id 线缆
    let resp = server.get(&format!("{base}/versions")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[0]["chars"].as_i64().unwrap() > 0, "字数从正文池统计");
    assert_eq!(
        body["latestUserVersionId"],
        json!(edited_id),
        "编辑版最后保存，是最新用户版本"
    );

    // 单版本正文
    let resp = server.get(&format!("{base}/versions/{imported_id}")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"], json!("导入的历史版本。"));

    // 版本选择：选中导入版 → effective 跟随；取消选择 → 回落最新用户版本
    let resp = server
        .post(
            &format!("{base}/content:select"),
            json!({ "versionId": imported_id }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["selectedVersionId"], json!(imported_id));
    let resp = server
        .post(
            &format!("{base}/content:select"),
            json!({ "versionId": null }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["selectedVersionId"], json!(null));
    assert_eq!(body["effectiveVersionId"], json!(edited_id));

    // diff：编辑版 vs 导入版
    let resp = server
        .get(&format!(
            "{base}/versions/{edited_id}/diff?against={imported_id}"
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["fromVersionId"], json!(edited_id));
    assert_eq!(body["toVersionId"], json!(imported_id));
    assert!(!body["ops"].as_array().unwrap().is_empty());

    // 取消编辑：回落上游原文
    let resp = server.delete(&format!("{base}/content")).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = server.get(&format!("{base}/content/edit")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    server.cleanup();
}

#[tokio::test]
async fn chapter_rename_and_marks() {
    let server = TestServer::start().await;
    let base = format!("/v1/books/{}/chapters/1", server.book_id);

    // 版本标记来自版本记录：先存一个编辑版
    let resp = server
        .put(
            &format!("{base}/content"),
            json!({ "content": "编辑版。
第二行。" }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 改名：覆盖层写入
    let resp = server
        .patch(base.as_str(), json!({ "title": "第二章（改）" }))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["renamed"], json!(true));
    assert_eq!(body["title"], json!("第二章（改）"));
    assert_eq!(body["hasOverride"], json!(true));

    // 覆盖状态（改名输入框初始态）
    let resp = server.get(&format!("{base}/title")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["hasOverride"], json!(true));

    // 版本标记：键是章节内部地址（过渡期例外）
    let resp = server
        .get(&format!("/v1/books/{}/chapter-marks", server.book_id))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let mark = &body["marks"][&server.chapter_url];
    assert_eq!(mark["kind"], json!("edited"), "编辑版应被标记");
    assert_eq!(mark["isLatestUpstream"], json!(false));

    // 与目录原标题相同的名字：按恢复原名处理
    let resp = server
        .patch(base.as_str(), json!({ "title": "第二章 继续" }))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["renamed"], json!(false));

    // 恢复原名（DELETE title）
    let resp = server
        .patch(base.as_str(), json!({ "title": "第二章（再改）" }))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = server.delete(&format!("{base}/title")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["hasOverride"], json!(false));
    assert_eq!(body["title"], json!("第二章 继续"));

    // 标题过长：400
    let resp = server
        .patch(base.as_str(), json!({ "title": "长".repeat(101) }))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    server.cleanup();
}

#[tokio::test]
async fn reset_book_to_upstream_clears_pointers() {
    let server = TestServer::start().await;

    // 两章各存一个编辑版
    for index in [0, 1] {
        let resp = server
            .put(
                &format!("/v1/books/{}/chapters/{index}/content", server.book_id),
                json!({ "content": format!("第 {index} 章编辑版。") }),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    let resp = server
        .post(
            &format!("/v1/books/{}/content:reset", server.book_id),
            json!({}),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["cleared"], json!(2));

    // 编辑版清掉（历史保留在时间线里）
    let resp = server
        .get(&format!(
            "/v1/books/{}/chapters/0/content/edit",
            server.book_id
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    server.cleanup();
}

#[tokio::test]
async fn edit_endpoints_require_auth() {
    let server = TestServer::start().await;
    let base = format!("/v1/books/{}/chapters/0", server.book_id);
    for path in [
        format!("{base}/content"),
        format!("{base}/content/edit"),
        format!("{base}/versions"),
        format!("{base}/title"),
        "/v1/books/book_1/chapter-marks".to_string(),
    ] {
        let resp = server
            .client
            .get(format!("{}{path}", server.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            resp.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
    }
    server.cleanup();
}
