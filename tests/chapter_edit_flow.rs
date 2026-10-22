//! 章节正文编辑版的端到端行为：
//! - 编辑版优先于一切缓存与上游（本地书原文切片 / 网络书 FileCache）；
//! - `refresh=1` 重抓上游更新缓存基准，但返回值仍是编辑版；
//! - 编辑版 diff 随正文一起返回（`data.content / data.edited / data.diff`）；
//! - 删除编辑版即恢复上游版本；删书级联清掉编辑版目录。

mod common;
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
    boot_with_git_path("").await
}

/// `git_path` 传不存在的路径可模拟「服务器没装 git」。
async fn boot_with_git_path(git_path: &str) -> TestServer {
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
        git_path: git_path.to_string(),
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

    async fn get_content(&self, book_id: &str, chapter_url: &str, extra: Value) -> Value {
        let mut body = json!({ "bookId": book_id, "chapterUrl": chapter_url, "index": 0 });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        self.post("getBookContent", body).await
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

    // 未编辑前：原文，不带 diff
    let before = server.get_content(book_id, &chapter_url, json!({})).await;
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

    // 阅读链路返回编辑版，并随正文带回与原文的 diff；清单能列出
    let after = server.get_content(book_id, &chapter_url, json!({})).await;
    assert_eq!(after["content"], json!("编辑后的整章正文。"));
    assert_eq!(after["edited"], json!(true));
    let ops = after["diff"]["ops"]
        .as_array()
        .unwrap_or_else(|| panic!("编辑版应带 diff: {after}"));
    let base_texts: Vec<&str> = ops
        .iter()
        .filter_map(|op| op["baseText"].as_str())
        .collect();
    assert!(
        base_texts
            .iter()
            .any(|text| text.contains("原始正文第一句")),
        "diff 基准应是原文: {after}"
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
    assert!(restored["content"]
        .as_str()
        .unwrap()
        .contains("原始正文第一句"));
    assert_eq!(restored["edited"], json!(false));
    assert!(restored.get("diff").is_none());

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

/// 路径注入防线：非法 bookId（`../..`、绝对路径、非数字）在 handler 层就 404/400，
/// service 层的 book_dir 兜底也只接受 `book_<n>`。
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

    // 批量删书携带非法 id：哨兵文件放在 data/ 根——若 "../.." 逃出
    // edited_chapters/<bookId> 落到上级目录，remove_dir_all 会连它带走
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
    // 再上传一本，带非法 id 一起删
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
        "非法 bookId 不应越出 edited_chapters 目录造成删除。剩余文件: {:?}",
        list_files_recursive(&server.temp_dir)
    );
    // 合法书的级联正常执行（编辑版已随书删除）
    let list = server
        .client
        .post(format!("{}/reader3/listChapterEdits", server.base_url))
        .bearer_auth(&server.token)
        .json(&json!({ "bookId": book_id }))
        .send()
        .await
        .unwrap();
    // 书已删，resolve_book 报 404
    assert_eq!(list.status(), StatusCode::NOT_FOUND);
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
    assert_eq!(first["content"], json!("上游第一版正文"));
    assert_eq!(first["edited"], json!(false));

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
    assert_eq!(edited["content"], json!("我的编辑版"));
    assert_eq!(edited["edited"], json!(true));

    // 上游出第二版 + refresh=1：基准更新到第二版，阅读仍是编辑版
    *version.lock().unwrap() = "上游第二版正文";
    let after_refresh = server
        .get_content(&book_id, &chapter_url, json!({ "refresh": 1 }))
        .await;
    assert_eq!(after_refresh["content"], json!("我的编辑版"));
    let base_texts: Vec<&str> = after_refresh["diff"]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|op| op["baseText"].as_str())
        .collect();
    assert!(
        base_texts
            .iter()
            .any(|text| text.contains("上游第二版正文")),
        "diff 基准应已更新到第二版: {after_refresh}"
    );
}

/// 服务器没有 git：正文照常返回，diff 缺省并带上原因，而不是整个请求失败。
#[tokio::test]
async fn content_stays_readable_when_git_is_missing() {
    let server = boot_with_git_path("definitely-not-a-git-binary").await;
    let book = server
        .upload_txt(
            "无 git 测试",
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
    assert!(
        payload.get("diff").is_none(),
        "没有 git 不该给 diff: {payload}"
    );
    let error = payload["diffError"].as_str().unwrap_or_default();
    assert!(error.contains("git"), "应说明缺 git: {payload}");
}

/// WebDAV 恢复重建登记表后：book_url 与同 id 新登记行不一致的编辑版与书签
/// 是恢复前别的书的残留，必须随 prune 清掉；地址一致的（同一本书）保留。
#[tokio::test]
async fn restore_prunes_edits_and_bookmarks_of_reused_book_ids() {
    let server = boot().await;
    let book = server.upload_txt("恢复甲", "第一章 开始\n正文。").await;
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let book_url = book["bookUrl"].as_str().unwrap().to_string();
    let chapter_url = format!("{book_url}#0");

    // 编辑一章 + 加一条书签
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
    server
        .post(
            "saveBookmark",
            json!({
                "bookId": book_id,
                "chapterIndex": 0,
                "chapterPos": 0,
                "chapterName": "第一章",
                "bookText": "正文",
            }),
        )
        .await;
    let edits = || async {
        server
            .post("listChapterEdits", json!({ "bookId": book_id }))
            .await["urls"]
            .as_array()
            .unwrap()
            .len()
    };
    let bookmarks = || async {
        let resp = server
            .client
            .get(format!("{}/reader3/getBookmarks", server.base_url))
            .bearer_auth(&server.token)
            .send()
            .await
            .unwrap();
        let payload: Value = resp.json().await.unwrap();
        payload["data"].as_array().unwrap().len()
    };
    assert_eq!(edits().await, 1);
    assert_eq!(bookmarks().await, 1);

    // 恢复一本别的书、占用同一个 bookId：旧书的编辑版与书签被清掉
    //（用 local-txt 地址冒充：resolve_book 对无书源的普通网络书会 404，
    //  而 prune 的判定只看登记表 book_url，与书源无关）
    server
        .post(
            "saveBooks",
            json!({
                "books": [{
                    "bookId": book_id,
                    "bookUrl": "local-txt:ffffffffffffffffffffffffffffffff",
                    "name": "别的书",
                    "origin": "local-txt",
                }],
                "restore": true,
            }),
        )
        .await;
    assert_eq!(edits().await, 0, "id 被别的书复用后旧编辑版应被清掉");
    assert_eq!(bookmarks().await, 0, "id 被别的书复用后旧书签应被清掉");
    assert!(
        !server
            .temp_dir
            .join("data")
            .join("user_1")
            .join("edited_chapters")
            .join(&book_id)
            .exists(),
        "编辑版正文目录应一并删除"
    );

    // 反向对照：恢复的同一本书（bookUrl 一致）时数据保留
    let book = server.upload_txt("恢复乙", "第二章 继续\n正文。").await;
    let book_id2 = book["bookId"].as_str().unwrap().to_string();
    let book_url2 = book["bookUrl"].as_str().unwrap().to_string();
    server
        .post(
            "saveChapterEdit",
            json!({
                "bookId": book_id2,
                "chapterUrl": format!("{book_url2}#0"),
                "chapterIndex": 0,
                "chapterTitle": "第二章",
                "content": "乙的编辑版",
            }),
        )
        .await;
    server
        .post(
            "saveBooks",
            json!({
                "books": [{
                    "bookId": book_id2,
                    "bookUrl": book_url2,
                    "name": "恢复乙",
                    "origin": "local-txt",
                }],
                "restore": true,
            }),
        )
        .await;
    let urls = server
        .post("listChapterEdits", json!({ "bookId": book_id2 }))
        .await;
    assert_eq!(
        urls["urls"].as_array().unwrap().len(),
        1,
        "同一本书恢复后编辑版应保留"
    );
}

/// 批量删书的级联按 canonical id 执行：别名（book_01）与 canonical 同效；
/// 合法格式但从未登记过的 id 不进级联（图片是全局存储，裸输入不能乱删）。
#[tokio::test]
async fn batch_delete_cascades_only_for_owned_books() {
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
    assert!(
        !server
            .temp_dir
            .join("data")
            .join("user_1")
            .join("edited_chapters")
            .join(&book_id)
            .exists(),
        "别名删除的级联应落到 canonical 目录"
    );
}

/// 书签必须挂在已登记的书上：单条保存 404，批量恢复跳过无主的书签
/// （空 book_url 的行是恢复校验的盲区，id 复用时会把书签挂到新书上）。
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
