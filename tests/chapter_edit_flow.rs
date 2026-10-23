//! 章节正文版本与编辑版的端到端行为：
//! - 用户版本（编辑/手动导入）优先于上游原文；
//! - `refresh=1` 追加新的上游版本，用户版本的 diff 基准保持不变；
//! - 版本时间线、任意版本 diff、跨请求的版本选择；
//! - 删除当前编辑只清指针，历史版本保留；删书也不删正文历史。

mod common;
use axum::{http::StatusCode, routing::get, Json, Router};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

async fn boot() -> TestServer {
    boot_with_options(24 * 3600).await
}

async fn boot_with_ttl(content_revalidate_ttl_secs: u64) -> TestServer {
    boot_with_options(content_revalidate_ttl_secs).await
}

async fn boot_with_options(content_revalidate_ttl_secs: u64) -> TestServer {
    common::clean_stale_temp_dirs();
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
        content_revalidate_ttl_secs,
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
        common::remove_dir_retried(&self.temp_dir);
    }
}

fn list_files_recursive(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.push(path.strip_prefix(base).unwrap().display().to_string());
            }
        }
    }
    walk(dir, dir, &mut out);
    out
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
        self.upload_txt_bytes(name, content.as_bytes()).await
    }

    async fn upload_txt_bytes(&self, name: &str, content: &[u8]) -> Value {
        // reqwest 主 crate 没开 multipart feature：手工拼 multipart body
        let boundary = "----chapter-edit-test";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}.txt\"\r\nContent-Type: text/plain\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(content);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
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

    async fn import_multipart(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        file: Option<&str>,
    ) -> Value {
        let boundary = "----chapter-import-test";
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
            .post(format!("{}/reader3/{path}", self.base_url))
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
        assert_eq!(
            payload["isSuccess"],
            json!(true),
            "{path} 应成功: {payload}"
        );
        payload["data"].clone()
    }

    async fn get_content(&self, book_id: &str, chapter_url: &str, extra: Value) -> Value {
        let mut body = json!({ "bookId": book_id, "chapterUrl": chapter_url, "index": 0 });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        self.post("getBookContent", body).await
    }

    async fn timeline(&self, book_id: &str, chapter_url: &str) -> Value {
        self.post(
            "getChapterTimeline",
            json!({ "bookId": book_id, "chapterUrl": chapter_url }),
        )
        .await
    }

    async fn list_edits(&self, book_id: &str) -> Value {
        self.post("listChapterEdits", json!({ "bookId": book_id }))
            .await
    }
}

fn item_version_id(item: &Value) -> i64 {
    item["versionId"].as_i64().unwrap()
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
    let book_id = book["bookId"].as_str().unwrap().to_string();

    // 本地书的章节 URL 形如 local-txt:<hash>（index 由参数带）
    let chapters = server
        .post("getChapterList", json!({ "bookId": book_id }))
        .await;
    let chapter_url = chapters[0]["url"].as_str().unwrap().to_string();

    // 未编辑前：原文，不带 diff
    let before = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert!(
        before["content"]
            .as_str()
            .unwrap()
            .contains("原始正文第一句"),
        "实际: {before}"
    );
    assert_eq!(before["edited"], json!(false));
    assert!(
        before.get("diff").is_none(),
        "没编辑过不该算 diff: {before}"
    );

    // 保存编辑版
    let saved = server
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
    let edit_version_id = saved["versionId"].as_i64().unwrap();

    // 阅读链路返回编辑版，并随正文带回与本地原文的 diff；清单能列出
    let after = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert_eq!(after["content"], json!("编辑后的整章正文。"));
    assert_eq!(after["edited"], json!(true));
    assert_eq!(after["versionId"], json!(edit_version_id));
    let ops = after["diff"]["ops"]
        .as_array()
        .unwrap_or_else(|| panic!("编辑版应带 diff: {after}"));
    assert!(
        ops.iter()
            .filter_map(|op| op["baseText"].as_str())
            .any(|text| text.contains("原始正文第一句")),
        "diff 基准应是本地原文: {after}"
    );

    // 时间线：一个编辑版，当前显示
    let timeline = server.timeline(&book_id, &chapter_url).await;
    let items = timeline["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "本地书只有用户版本: {timeline}");
    assert_eq!(items[0]["kind"], json!("edited"));
    assert_eq!(items[0]["isCurrent"], json!(true));

    // 上游预览返回本地原文；阅读页不为上游预览计算用户 diff
    let upstream_view = server
        .get_content(&book_id, &chapter_url, json!({ "variant": "upstream" }))
        .await;
    assert_eq!(upstream_view["variant"], json!("upstream"));
    assert_eq!(upstream_view["edited"], json!(false));
    assert!(
        upstream_view["content"]
            .as_str()
            .unwrap()
            .contains("原始正文第一句"),
        "上游原版应返回原文: {upstream_view}"
    );
    assert!(
        upstream_view.get("diff").is_none(),
        "上游预览不显示用户 diff: {upstream_view}"
    );

    let list = server.list_edits(&book_id).await;
    assert_eq!(list["urls"].as_array().unwrap().len(), 1);

    // 删除当前编辑 → 恢复原文；历史版本仍然可读
    server
        .post(
            "deleteChapterEdit",
            json!({ "bookId": book_id, "chapterUrl": chapter_url }),
        )
        .await;
    let restored = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert!(restored["content"]
        .as_str()
        .unwrap()
        .contains("原始正文第一句"));
    assert_eq!(restored["edited"], json!(false));
    assert!(restored.get("diff").is_none());
    assert_eq!(
        server.list_edits(&book_id).await["urls"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "清掉当前编辑后目录徽章不应再显示"
    );
    let history = server
        .post(
            "getChapterVersion",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "versionId": edit_version_id,
            }),
        )
        .await;
    assert_eq!(history["content"], json!("编辑后的整章正文。"));

    // 再编辑后从书架删除：登记行与正文历史保留，重新加入还能接上
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
    assert_eq!(
        server.list_edits(&book_id).await["urls"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "删书只移出书架，正文历史不删"
    );
}

/// 路径注入防线：非法 bookId 在 handler 层就 404/400，模板/文件路径不会被拼出去。
#[tokio::test]
async fn path_traversal_book_ids_are_rejected() {
    let server = boot().await;
    let book = server.upload_txt("防线测试", "第一章 开始\n正文。\n").await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    server
        .post(
            "saveChapterEdit",
            json!({
                "bookId": book_id,
                "chapterUrl": "local-txt:x",
                "chapterIndex": 0,
                "chapterTitle": "x",
                "content": "有了一条编辑版",
            }),
        )
        .await;

    let evil_ids = [
        "../../user_2",
        "..\\..\\",
        "/etc",
        "book_abc",
        "book_1/../../",
    ];
    for evil in evil_ids {
        for (path, body) in [
            (
                "saveChapterEdit",
                json!({"bookId": evil, "chapterUrl": "x", "content": "y"}),
            ),
            ("listChapterEdits", json!({"bookId": evil})),
            (
                "searchCachedContent",
                json!({"bookId": evil, "query": "词"}),
            ),
        ] {
            let resp = server
                .client
                .post(format!("{}/reader3/{path}", server.base_url))
                .bearer_auth(&server.token)
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = resp.status();
            assert!(
                status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST,
                "{path} 对 {evil:?} 应 404/400，实际 {status}"
            );
        }
    }

    // 批量删书携带非法 id：哨兵文件放在 data/ 根，路径拼接若越界会把它带走
    let sentinel = server.temp_dir.join("data").join("sentinel.txt");
    std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    std::fs::write(&sentinel, "别删我").unwrap();
    server
        .post("deleteBooks", json!({ "bookIds": [book_id.clone()] }))
        .await;
    assert!(
        sentinel.exists(),
        "合法批量删书不应带走 data/ 根。剩余文件: {:?}",
        list_files_recursive(&server.temp_dir)
    );
    let book2 = server
        .upload_txt("防线测试2", "第一章 开始\n正文。\n")
        .await;
    server
        .post(
            "deleteBooks",
            json!({ "bookIds": [book2["bookId"].as_str().unwrap(), "../..", "../../.."] }),
        )
        .await;
    assert!(
        sentinel.exists(),
        "非法 bookId 不应越界删除。剩余文件: {:?}",
        list_files_recursive(&server.temp_dir)
    );
    // 删书只移出书架，登记行仍在，历史接口仍能解析
    assert_eq!(
        server.list_edits(&book_id).await["urls"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// 网络书：上游更新追加新版本，编辑版基准不变；时间线与任意版本 diff 可用。
#[tokio::test]
async fn network_book_versions_and_diff() {
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

    // 首抓：上游第一版成为第一个版本
    let first = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert_eq!(first["content"], json!("上游第一版正文"));
    assert_eq!(first["versionKind"], json!("upstream"));
    let first_upstream_id = first["versionId"].as_i64().unwrap();

    // 保存编辑版：基准记为第一版
    let saved = server
        .post(
            "saveChapterEdit",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "chapterIndex": 0,
                "chapterTitle": "第一章",
                "content": "我的编辑版",
                "baseVersionId": first_upstream_id,
            }),
        )
        .await;
    let edit_version_id = saved["versionId"].as_i64().unwrap();
    let edited = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert_eq!(edited["content"], json!("我的编辑版"));
    assert_eq!(edited["edited"], json!(true));
    assert_eq!(edited["baseVersionId"], json!(first_upstream_id));

    // 上游出第二版 + refresh=1：baseVersionId 仍是第一版，但阅读页 diff
    // 始终以最新上游（第二版）为基准，方便和评论段号对齐
    *version.lock().unwrap() = "上游第二版正文";
    let after_refresh = server
        .get_content(&book_id, &chapter_url, json!({ "refresh": 1 }))
        .await;
    assert_eq!(after_refresh["content"], json!("我的编辑版"));
    assert_eq!(after_refresh["baseVersionId"], json!(first_upstream_id));
    assert_eq!(after_refresh["upstreamChanged"], json!(true));
    let latest_upstream_id = after_refresh["latestUpstreamVersionId"].as_i64().unwrap();
    assert_ne!(latest_upstream_id, first_upstream_id);
    assert_eq!(
        after_refresh["diffBaseVersionId"],
        json!(latest_upstream_id),
        "阅读页 diff 基准必须是最新上游"
    );
    let base_texts: Vec<&str> = after_refresh["diff"]["ops"]
        .as_array()
        .unwrap_or_else(|| panic!("编辑版应带 diff: {after_refresh}"))
        .iter()
        .filter_map(|op| op["baseText"].as_str())
        .collect();
    assert!(
        base_texts
            .iter()
            .any(|text| text.contains("上游第二版正文")),
        "阅读页 diff 应对比最新上游: {after_refresh}"
    );

    // 上游预览返回第二版，不改变当前选择
    let upstream_view = server
        .get_content(&book_id, &chapter_url, json!({ "variant": "upstream" }))
        .await;
    assert_eq!(upstream_view["variant"], json!("upstream"));
    assert_eq!(upstream_view["content"], json!("上游第二版正文"));
    assert_eq!(upstream_view["edited"], json!(false));

    // 时间线：上游 v1、编辑版、上游 v2 三个版本
    let timeline = server.timeline(&book_id, &chapter_url).await;
    let items = timeline["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "时间线应保留全部版本: {timeline}");
    let upstream_v1 = items
        .iter()
        .find(|item| item_version_id(item) == first_upstream_id)
        .expect("第一版应仍在时间线");
    let upstream_v2 = items
        .iter()
        .find(|item| item_version_id(item) == latest_upstream_id)
        .expect("第二版应仍在时间线");
    assert_eq!(upstream_v2["isLatestUpstream"], json!(true));
    assert_eq!(upstream_v1["isLatestUser"], json!(false));

    // 任意两个版本的 diff
    let diff = server
        .post(
            "getChapterVersionDiff",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "fromVersionId": first_upstream_id,
                "toVersionId": latest_upstream_id,
            }),
        )
        .await;
    let ops = diff["ops"].as_array().unwrap();
    let texts: Vec<&str> = ops
        .iter()
        .flat_map(|op| {
            [
                op["baseText"].as_str().unwrap_or_default(),
                op["editedText"].as_str().unwrap_or_default(),
            ]
        })
        .collect();
    assert!(texts.iter().any(|text| text.contains("上游第一版正文")));
    assert!(texts.iter().any(|text| text.contains("上游第二版正文")));

    // 跨请求的版本选择：选第一版后默认显示第一版，再恢复跟随最新
    server
        .post(
            "selectChapterVersion",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "versionId": first_upstream_id,
            }),
        )
        .await;
    let selected = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert_eq!(selected["content"], json!("上游第一版正文"));
    assert_eq!(selected["selectionSource"], json!("selected"));
    assert_eq!(selected["edited"], json!(false));

    server
        .post(
            "selectChapterVersion",
            json!({ "bookId": book_id, "chapterUrl": chapter_url }),
        )
        .await;
    let follow_latest = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert_eq!(follow_latest["content"], json!("我的编辑版"));
    assert_eq!(follow_latest["selectionSource"], json!("edited"));

    // 删除当前编辑：回落最新上游，历史仍保留
    server
        .post(
            "deleteChapterEdit",
            json!({ "bookId": book_id, "chapterUrl": chapter_url }),
        )
        .await;
    let after_delete = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert_eq!(after_delete["content"], json!("上游第二版正文"));
    assert_eq!(after_delete["edited"], json!(false));
    assert_eq!(
        server.timeline(&book_id, &chapter_url).await["items"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "删除当前编辑不删历史"
    );
    let history = server
        .post(
            "getChapterVersion",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "versionId": edit_version_id,
            }),
        )
        .await;
    assert_eq!(history["content"], json!("我的编辑版"));
}

/// diff 由服务端进程内计算（imara-diff），不依赖任何外部工具：
/// 保存编辑版后 getBookContent 直接给出差异，不再有降级路径。
#[tokio::test]
async fn diff_is_available_without_external_tools() {
    let server = boot().await;
    let book = server
        .upload_txt(
            "进程内 diff 测试",
            "第一章 开始
原始正文。
",
        )
        .await;
    let book_id = book["bookId"].as_str().unwrap();
    let chapters = server
        .post("getChapterList", json!({ "bookId": book_id }))
        .await;
    let chapter_url = chapters[0]["url"].as_str().unwrap().to_string();
    server
        .post(
            "saveChapterEdit",
            json!({
                "bookId": book_id,
                "chapterUrl": chapter_url,
                "chapterIndex": 0,
                "chapterTitle": "第一章 开始",
                "content": "改过的正文。",
            }),
        )
        .await;

    let payload = server.get_content(book_id, &chapter_url, json!({})).await;
    assert_eq!(payload["content"], json!("改过的正文。"));
    assert_eq!(payload["edited"], json!(true));
    let ops = payload["diff"]["ops"]
        .as_array()
        .unwrap_or_else(|| panic!("进程内 diff 永远可用: {payload}"));
    assert!(!ops.is_empty(), "编辑版与上游不同就该有差异: {payload}");
    assert_eq!(
        payload["diffError"].as_str().unwrap_or_default(),
        "",
        "不该有 diffError: {payload}"
    );
}

/// 超过回源间隔：当前请求先返回旧缓存，后台抓到新版本后下一次读取更新。
#[tokio::test]
async fn stale_chapter_revalidates_in_background() {
    let version = Arc::new(Mutex::new("上游第一版"));
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

    let server = boot_with_ttl(1).await;
    server
        .post(
            "saveBookSource",
            json!({
                "bookSourceName": "回源测试源",
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
                "name": "回源测试",
                "author": "作者",
                "bookUrl": format!("{upstream}/info"),
                "bookSourceId": source_id,
            }),
        )
        .await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let chapter_url = format!("{upstream}/content");

    assert_eq!(
        server.get_content(&book_id, &chapter_url, json!({})).await["content"],
        json!("上游第一版")
    );

    *version.lock().unwrap() = "上游第二版";
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    // 到期的第一次读取先返回旧缓存，并标记 stale
    let stale = server.get_content(&book_id, &chapter_url, json!({})).await;
    assert_eq!(stale["content"], json!("上游第一版"));
    assert_eq!(stale["stale"], json!(true));

    // 后台任务完成后，下一次读取看到第二版
    let mut latest = stale;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        latest = server.get_content(&book_id, &chapter_url, json!({})).await;
        if latest["content"] == json!("上游第二版") {
            break;
        }
    }
    assert_eq!(latest["content"], json!("上游第二版"));
    assert_eq!(latest["stale"], json!(false));

    let timeline = server.timeline(&book_id, &chapter_url).await;
    let upstream_versions = timeline["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["kind"] == json!("upstream"))
        .count();
    assert_eq!(upstream_versions, 2, "两次成功抓取都应保留");
}

/// 空正文不进时间线：文本源直接报错；图片源只记检查时间，不追加 0 字节版本。
#[tokio::test]
async fn empty_upstream_content_is_not_stored_as_a_version() {
    let hits = Arc::new(AtomicUsize::new(0));
    let upstream = {
        let hits = hits.clone();
        let app = Router::new().route(
            "/content",
            get(move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    Json(json!({ "data": { "content": "" } }))
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
    let source_body = |name: &str, source_url: &str, kind: i32| {
        json!({
            "bookSourceName": name,
            "bookSourceUrl": source_url,
            "enabled": true,
            "bookSourceType": kind,
            "ruleContent": { "content": "$.data.content" },
            "ruleContentImage": {
                "listRule": "$.images[*]",
                "imageUrl": "image?item_id={{$.data.item_id}}"
            }
        })
    };
    server
        .post("saveBookSource", source_body("空文本源", &upstream, 0))
        .await;
    server
        .post(
            "saveBookSource",
            source_body("空图片源", &format!("{upstream}/image-source"), 2),
        )
        .await;
    let sources = server.post("getBookSources", json!({})).await;
    let source_id = |name: &str| {
        sources
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["bookSourceName"] == json!(name))
            .unwrap()["bookSourceId"]
            .as_i64()
            .unwrap()
    };
    let text_source_id = source_id("空文本源");
    let image_source_id = source_id("空图片源");
    let chapter_url = format!("{upstream}/content");

    let text_book = server
        .post(
            "saveBook",
            json!({
                "name": "空文本",
                "author": "作者",
                "bookUrl": format!("{upstream}/info"),
                "bookSourceId": text_source_id,
            }),
        )
        .await;
    let text_book_id = text_book["bookId"].as_str().unwrap().to_string();
    let resp = server
        .client
        .post(format!("{}/reader3/getBookContent", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({
            "bookId": text_book_id,
            "chapterUrl": chapter_url,
            "index": 0,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "文本源空正文应报错");
    let payload: Value = resp.json().await.unwrap();
    assert!(
        payload["errorMsg"]
            .as_str()
            .unwrap_or_default()
            .contains("正文为空"),
        "{payload}"
    );
    assert_eq!(
        server.timeline(&text_book_id, &chapter_url).await["items"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "文本源的 0 字节正文不应进时间线"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let image_book = server
        .post(
            "saveBook",
            json!({
                "name": "空图片",
                "author": "作者",
                "bookUrl": format!("{upstream}/info2"),
                "bookSourceId": image_source_id,
            }),
        )
        .await;
    let image_book_id = image_book["bookId"].as_str().unwrap().to_string();
    let empty = server
        .get_content(&image_book_id, &chapter_url, json!({}))
        .await;
    assert_eq!(empty["content"], json!(""), "图片源允许空正文: {empty}");
    assert_eq!(
        server.timeline(&image_book_id, &chapter_url).await["items"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "图片源的空正文也不进时间线"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    // 回源间隔内再读：直接返回空正文，不再抓上游
    let again = server
        .get_content(&image_book_id, &chapter_url, json!({}))
        .await;
    assert_eq!(again["content"], json!(""));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "空正文在回源间隔内不应重复抓取"
    );
}

/// 恢复备份只替换书架，不删除全局登记行与正文历史。
#[tokio::test]
async fn restore_keeps_registration_and_edit_history() {
    let server = boot().await;
    let book = server.upload_txt("恢复甲", "第一章 开始\n正文。").await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let book_url = book["bookUrl"].as_str().unwrap().to_string();
    let chapter_url = format!("{book_url}#0");

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
    assert_eq!(
        server.list_edits(&book_id).await["urls"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // 全量恢复同一本书：书架重建，登记行与编辑历史保留
    server
        .post(
            "saveBooks",
            json!({
                "books": [{
                    "bookId": book_id,
                    "bookUrl": book_url,
                    "name": "恢复甲",
                    "origin": "local-txt",
                }],
                "restore": true,
            }),
        )
        .await;
    assert_eq!(
        server.list_edits(&book_id).await["urls"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "恢复备份不应删除正文历史"
    );
}

/// 批量删书：别名与 canonical 同效；登记行和正文历史保留，只移出书架。
#[tokio::test]
async fn batch_delete_keeps_history() {
    let server = boot().await;
    let book = server.upload_txt("级联归属", "第一章 开始\n正文。\n").await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    server
        .post(
            "saveChapterEdit",
            json!({
                "bookId": book_id,
                "chapterUrl": "local-txt:x",
                "chapterIndex": 0,
                "chapterTitle": "x",
                "content": "有了一条编辑版",
            }),
        )
        .await;

    let alias = format!("book_0{}", book_id.trim_start_matches("book_"));
    let data = server
        .post(
            "deleteBooks",
            json!({ "bookIds": [alias, "book_999", "../.."] }),
        )
        .await;
    assert_eq!(data["deleted"], json!(1), "只有别名指向的那本真被删");
    assert_eq!(
        server.list_edits(&book_id).await["urls"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "删书只移出书架，编辑历史保留"
    );
}

/// 整本 TXT：按标题/序号匹配目录，预览可查看，确认后批量写入。
#[tokio::test]
async fn whole_book_text_preview_and_batch_import() {
    let server = boot().await;
    let book = server
        .upload_txt(
            "整本导入测试",
            "第一章 开始\n本地正文一。\n第二章 继续\n本地正文二。\n第三章 结束\n本地正文三。\n",
        )
        .await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let chapters = server
        .post("getChapterList", json!({ "bookId": book_id }))
        .await;
    let first_url = chapters[0]["url"].as_str().unwrap().to_string();
    let second_url = chapters[1]["url"].as_str().unwrap().to_string();
    let third_url = chapters[2]["url"].as_str().unwrap().to_string();

    // 上传的 TXT 里有两章能匹配，另有一个目录里不存在的「第三十七章」。
    let text = "第一章 开始\n导入正文一。\n第二章 继续\n导入正文二。\n第三十七章 不存在\n这段不应该匹配。\n";
    let preview = server
        .import_multipart(
            "previewChapterImport",
            &[("bookId", &book_id), ("mode", "book")],
            Some(text),
        )
        .await;
    assert_eq!(preview["mode"], json!("book"));
    assert_eq!(preview["total"], json!(3));
    assert_eq!(preview["matched"], json!(2));
    assert_eq!(
        preview["unmatched"],
        json!(1),
        "目录序号对不上的章节不硬配: {preview}"
    );
    assert_eq!(preview["cached"], json!(0), "新书还没有正文缓存: {preview}");
    assert_eq!(preview["uncached"], json!(2));
    assert_eq!(preview["changed"], json!(0));
    assert!(
        preview["previewId"].as_str().is_some(),
        "预览应生成详情对比令牌: {preview}"
    );
    let items = preview["chapters"].as_array().unwrap();
    assert_eq!(items[0]["chapterUrl"], json!(first_url));
    assert_eq!(items[1]["chapterUrl"], json!(second_url));
    assert_eq!(items[2]["status"], json!("unmatched"));
    assert_eq!(items[0]["matchKind"], json!("title"));
    assert_eq!(items[0]["cached"], json!(false));
    assert_eq!(items[0]["changed"], json!(false));

    // 只导入前两章；第三十七章不选，也不该被塞进第三章
    let result = server
        .import_multipart(
            "importChapterText",
            &[
                ("bookId", &book_id),
                ("mode", "book"),
                ("kind", "imported"),
                ("note", "整本导入测试"),
                ("makeCurrent", "true"),
                ("selectedIndexes", "0,1"),
            ],
            Some(text),
        )
        .await;
    assert_eq!(result["imported"], json!(2), "{result}");
    assert_eq!(result["failed"], json!(0), "{result}");

    let first = server.get_content(&book_id, &first_url, json!({})).await;
    assert_eq!(first["content"], json!("导入正文一。"));
    assert_eq!(first["edited"], json!(true));
    let first_timeline = server.timeline(&book_id, &first_url).await;
    let first_item = &first_timeline["items"].as_array().unwrap()[0];
    assert_eq!(first_item["kind"], json!("imported"));
    assert_eq!(first_item["note"], json!("整本导入测试"));

    let second = server.get_content(&book_id, &second_url, json!({})).await;
    assert_eq!(second["content"], json!("导入正文二。"));

    // 第三章保持原样：没有因为多出来的第三十七章被覆盖
    let third = server.get_content(&book_id, &third_url, json!({})).await;
    assert_eq!(third["edited"], json!(false));
    assert!(third["content"].as_str().unwrap().contains("本地正文三"));
}

/// 单章 TXT：粘贴带标题的正文，导入时去掉标题行，只写当前章节。
#[tokio::test]
async fn single_chapter_text_import_strips_heading() {
    let server = boot().await;
    let book = server
        .upload_txt(
            "单章导入测试",
            "第一章 开始\n本地正文一。\n第二章 继续\n本地正文二。\n",
        )
        .await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let chapters = server
        .post("getChapterList", json!({ "bookId": book_id }))
        .await;
    let second_url = chapters[1]["url"].as_str().unwrap().to_string();

    let text = "第二章 继续\n这是单章导入的正文。\n第二段。";
    // 用 text 字段提交（粘贴路径），文件路径由上一个用例覆盖
    let preview = server
        .import_multipart(
            "previewChapterImport",
            &[
                ("bookId", &book_id),
                ("mode", "single"),
                ("chapterUrl", &second_url),
                ("chapterIndex", "1"),
                ("chapterTitle", "第二章 继续"),
                ("text", text),
            ],
            None,
        )
        .await;
    assert_eq!(preview["mode"], json!("single"));
    assert_eq!(preview["total"], json!(1));
    assert_eq!(preview["matched"], json!(1));
    assert_eq!(preview["chapters"][0]["title"], json!("第二章 继续"));
    assert_eq!(preview["chapters"][0]["matchKind"], json!("single"));

    let result = server
        .import_multipart(
            "importChapterText",
            &[
                ("bookId", &book_id),
                ("mode", "single"),
                ("chapterUrl", &second_url),
                ("chapterIndex", "1"),
                ("chapterTitle", "第二章 继续"),
                ("makeCurrent", "true"),
                ("text", text),
            ],
            None,
        )
        .await;
    assert_eq!(result["imported"], json!(1), "{result}");

    let content = server.get_content(&book_id, &second_url, json!({})).await;
    assert_eq!(content["content"], json!("这是单章导入的正文。\n第二段。"));
    assert_eq!(content["versionKind"], json!("edited"));
}

/// 预览拿当前正文对比：没变化的章节默认不需要导入，详情能按需看差异。
#[tokio::test]
async fn import_preview_marks_changed_chapters_and_serves_detail_diff() {
    let server = boot().await;
    let book = server
        .upload_txt(
            "导入差异测试",
            "第一章 开始\n本地正文一。\n第二章 继续\n本地正文二。\n第三章 结束\n本地正文三。\n",
        )
        .await;
    let book_id = book["bookId"].as_str().unwrap().to_string();

    let text =
        "第一章 开始\n第一次导入一。\n第二章 继续\n第一次导入二。\n第三章 结束\n第一次导入三。";
    let imported = server
        .import_multipart(
            "importChapterText",
            &[
                ("bookId", &book_id),
                ("mode", "book"),
                ("makeCurrent", "true"),
            ],
            Some(text),
        )
        .await;
    assert_eq!(imported["imported"], json!(3), "{imported}");

    // 同一份文本再预览：三章都已有缓存，且都与当前正文一致
    let same = server
        .import_multipart(
            "previewChapterImport",
            &[("bookId", &book_id), ("mode", "book")],
            Some(text),
        )
        .await;
    assert_eq!(same["matched"], json!(3));
    assert_eq!(same["cached"], json!(3), "{same}");
    assert_eq!(same["changed"], json!(0), "{same}");
    assert!(same["chapters"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["changed"] == json!(false)));
    let same_preview_id = same["previewId"].as_str().unwrap().to_string();
    let same_diff = server
        .post(
            "getChapterImportDiff",
            json!({ "previewId": same_preview_id, "sourceIndex": 0 }),
        )
        .await;
    assert_eq!(same_diff["changed"], json!(false), "{same_diff}");
    assert_eq!(same_diff["ops"], json!([]));

    // 改第一章后再预览：只有第一章标记有差异
    let changed_text =
        "第一章 开始\n第二次导入一。\n第二章 继续\n第一次导入二。\n第三章 结束\n第一次导入三。";
    let preview = server
        .import_multipart(
            "previewChapterImport",
            &[("bookId", &book_id), ("mode", "book")],
            Some(changed_text),
        )
        .await;
    assert_eq!(preview["matched"], json!(3));
    assert_eq!(preview["cached"], json!(3));
    assert_eq!(preview["changed"], json!(1), "{preview}");
    let items = preview["chapters"].as_array().unwrap();
    assert_eq!(items[0]["changed"], json!(true));
    assert_eq!(items[1]["changed"], json!(false));

    let preview_id = preview["previewId"].as_str().unwrap().to_string();
    let diff = server
        .post(
            "getChapterImportDiff",
            json!({ "previewId": preview_id, "sourceIndex": 0 }),
        )
        .await;
    assert_eq!(diff["changed"], json!(true), "{diff}");
    let texts: Vec<&str> = diff["ops"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|op| {
            [
                op["baseText"].as_str().unwrap_or_default(),
                op["editedText"].as_str().unwrap_or_default(),
            ]
        })
        .collect();
    assert!(
        texts.iter().any(|text| text.contains("第一次导入一")),
        "差异应包含当前正文: {diff}"
    );
    assert!(
        texts.iter().any(|text| text.contains("第二次导入一")),
        "差异应包含导入正文: {diff}"
    );
    assert_eq!(
        diff["importChars"],
        json!(items[0]["chars"].as_u64().unwrap()),
        "{diff}"
    );
    assert!(
        diff["currentExcerpt"]
            .as_str()
            .unwrap_or_default()
            .contains("第一次导入一"),
        "{diff}"
    );
}

/// 书签必须挂在已登记的书上：单条保存 404，批量恢复跳过无主的书签。
#[tokio::test]
async fn bookmarks_require_a_registered_book() {
    let server = boot().await;
    let book = server.upload_txt("书签归属", "第一章 开始\n正文。\n").await;
    let book_id = book["bookId"].as_str().unwrap().to_string();

    // 未登记的书：单条保存 404
    let resp = server
        .client
        .post(format!("{}/reader3/saveBookmark", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "bookId": "book_999", "chapterIndex": 0, "chapterPos": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 批量：有主的落库、无主的跳过（不因一条脏数据整批失败）
    server
        .post(
            "saveBookmarks",
            json!([
                { "bookId": book_id, "chapterIndex": 0, "chapterPos": 0, "chapterName": "第一章", "bookText": "正文" },
                { "bookId": "book_999", "chapterIndex": 1, "chapterPos": 0, "bookText": "孤儿" },
            ]),
        )
        .await;
    let resp = server
        .client
        .get(format!("{}/reader3/getBookmarks", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    let payload: Value = resp.json().await.unwrap();
    let list = payload["data"].as_array().unwrap();
    assert_eq!(list.len(), 1, "无主书签不该落库");
    assert_eq!(list[0]["bookId"], json!(book_id));
}
