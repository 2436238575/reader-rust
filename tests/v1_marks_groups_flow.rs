//! `/v1/me/marks`（书签/划线）与 `/v1/me/groups`（书架分组）+ 书架 PATCH 的
//! `group` 字段：身份五元组、批量恢复的跳过口径、位掩码分组与删组清位。

use axum::http::StatusCode;
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
    pool: sqlx::SqlitePool,
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-marks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = common::base_config(&temp_dir, "v1-marks-test-secret");
        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let (base_url, _server) = common::spawn_app(app).await;
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
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    /// 预置登记书（不入架）。
    async fn seed_registered_book(&self, url: &str) -> String {
        let source_id: i64 = sqlx::query_scalar(
            "INSERT INTO book_sources (book_source_url, book_source_name, json, updated_at)              VALUES (?1, '测试源', '{}', 0) RETURNING book_source_id",
        )
        .bind(url)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let book_id: i64 = sqlx::query_scalar(
            "INSERT INTO books (book_url, book_source_id, name, author, created_at, updated_at)              VALUES (?1, ?2, '测试书', '作者', 0, 0) RETURNING book_id",
        )
        .bind(format!("{url}/book"))
        .bind(source_id)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        format!("book_{book_id}")
    }

    /// 预置登记书 + 在架书架行（分组的归属载体）。
    async fn seed_shelf_book(&self, url: &str) -> String {
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
        sqlx::query(
            "INSERT INTO shelf_books (user_ns, book_id, position, created_at, updated_at) \
             VALUES ('user_1', ?1, 0, 0, 0)",
        )
        .bind(book_id)
        .execute(&self.pool)
        .await
        .unwrap();
        format!("book_{book_id}")
    }

    fn mark(&self, book_id: &str, chapter_index: i32, chapter_pos: i32, kind: &str) -> Value {
        json!({
            "bookId": book_id,
            "time": 1_760_000_000i64,
            "bookName": "测试书",
            "bookAuthor": "作者",
            "chapterIndex": chapter_index,
            "chapterPos": chapter_pos,
            "chapterName": format!("第 {chapter_index} 章"),
            "bookText": "正文片段",
            "content": "划线内容",
            "kind": kind
        })
    }

    async fn post_json(&self, path: &str, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn patch_json(&self, path: &str, body: Value) -> reqwest::Response {
        self.client
            .patch(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
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
async fn marks_roundtrip_and_batch_skips_unknown_books() {
    let server = TestServer::start().await;
    let book_1 = server.seed_shelf_book("https://up1.example.com").await;
    let book_2 = server.seed_shelf_book("https://up2.example.com").await;

    // 单条保存：204，同身份覆盖
    let resp = server
        .post_json("/v1/me/marks", server.mark(&book_1, 0, 0, "bookmark"))
        .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = server
        .post_json("/v1/me/marks", server.mark(&book_1, 1, 5000, "underline"))
        .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 列表：全部 + 按书过滤
    let body: Value = server.get("/v1/me/marks").await.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 2);
    let body: Value = server
        .get(&format!("/v1/me/marks?bookId={book_2}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body.as_array().unwrap().len(), 0);

    // 同身份再保存：覆盖不新增，内容取新值
    let mut updated = server.mark(&book_1, 0, 0, "bookmark");
    updated["content"] = json!("更新后的内容");
    let resp = server.post_json("/v1/me/marks", updated).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let body: Value = server.get("/v1/me/marks").await.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 2);
    let first = body
        .as_array()
        .unwrap()
        .iter()
        .find(|mark| mark["chapterIndex"] == json!(0))
        .unwrap();
    assert_eq!(first["content"], json!("更新后的内容"));

    // 批量保存：有效 1 条 + 指向未登记书 1 条 → saved 1 / skipped 1
    let resp = server
        .post_json(
            "/v1/me/marks:batch",
            json!([
                server.mark(&book_2, 2, 0, "bookmark"),
                server.mark("book_999999", 0, 0, "bookmark")
            ]),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["saved"], json!(1));
    assert_eq!(body["skipped"], json!(1));

    // 未登记书的单条保存：404 精确码；空 bookId：400
    let resp = server
        .post_json("/v1/me/marks", server.mark("book_999999", 0, 0, "bookmark"))
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("BOOK_NOT_FOUND"));
    let mut no_book = server.mark(&book_1, 0, 0, "bookmark");
    no_book["bookId"] = json!("  ");
    let resp = server.post_json("/v1/me/marks", no_book).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 批量删除：按身份五元组
    let resp = server
        .post_json(
            "/v1/me/marks:batchDelete",
            json!([server.mark(&book_1, 0, 0, "bookmark")]),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["deleted"], json!(1));
    let body: Value = server.get("/v1/me/marks").await.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 2);

    server.cleanup();
}

#[tokio::test]
async fn groups_crud_and_shelf_group_patch() {
    let server = TestServer::start().await;
    let book_id = server.seed_shelf_book("https://up.example.com").await;

    // 新建：groupId=0 由服务端分配空位幂
    let resp = server
        .post_json(
            "/v1/me/groups",
            json!({ "groupId": 0, "groupName": "默认" }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let body: Value = server.get("/v1/me/groups").await.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);
    let allocated = body[0]["groupId"].as_i64().unwrap();
    assert_eq!(allocated & (allocated - 1), 0, "分配的是 2 的幂");

    // 指定幂位新建 + 非幂拒绝
    let resp = server
        .post_json("/v1/me/groups", json!({ "groupId": 4, "groupName": "四" }))
        .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = server
        .post_json("/v1/me/groups", json!({ "groupId": 3, "groupName": "三" }))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 排序：只改 orderNo
    let resp = server
        .patch_json(
            "/v1/me/groups:order",
            json!([{ "groupId": 4, "orderNo": 5 }]),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let body: Value = server.get("/v1/me/groups").await.json().await.unwrap();
    let group_four = body
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["groupId"] == json!(4))
        .unwrap();
    assert_eq!(group_four["orderNo"], json!(5));

    // 书架 PATCH group：指定已存在的组
    let resp = server
        .patch_json(&format!("/v1/me/shelf/{book_id}"), json!({ "group": 4 }))
        .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let body: Value = server
        .get(&format!("/v1/me/shelf/{book_id}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["group"], json!(4));

    // 非幂位：400；合法幂位但组不存在：422 带 pointer
    let resp = server
        .patch_json(&format!("/v1/me/shelf/{book_id}"), json!({ "group": 999 }))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = server
        .patch_json(&format!("/v1/me/shelf/{book_id}"), json!({ "group": 1024 }))
        .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["errors"][0]["pointer"], json!("/group"));

    // group 与 progress 相互独立：只传 progress 不动分组
    let resp = server
        .patch_json(
            &format!("/v1/me/shelf/{book_id}"),
            json!({ "progress": { "chapterIndex": 1, "offset": 0.5 } }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let body: Value = server
        .get(&format!("/v1/me/shelf/{book_id}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["group"], json!(4));

    // 删组：位掩码清位 → 书的分组归 0
    let resp = server
        .client
        .delete(format!("{}/v1/me/groups/4", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let body: Value = server
        .get(&format!("/v1/me/shelf/{book_id}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["group"], json!(0), "删组应清掉书上的对应位");

    // 非法幂位删除：400
    let resp = server
        .client
        .delete(format!("{}/v1/me/groups/3", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    server.cleanup();
}

#[tokio::test]
async fn shelf_group_patch_requires_shelf_entry() {
    let server = TestServer::start().await;
    let book_id = server.seed_registered_book("https://up.example.com").await;
    // 组先存在（否则先撞 422 分组不存在，轮不到书架校验）
    server
        .post_json(
            "/v1/me/groups",
            json!({ "groupId": 1, "groupName": "组一" }),
        )
        .await;

    // 已登记但不在书架：409 BOOK_NOT_IN_SHELF
    let resp = server
        .patch_json(&format!("/v1/me/shelf/{book_id}"), json!({ "group": 1 }))
        .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("BOOK_NOT_IN_SHELF"));

    server.cleanup();
}

/// 批量保存的请求体上限真实可达：2000 条带选文的划线约 2MB，超过 /v1 默认
/// 1MB——路由级 8MB 必须生效（否则 413，且 http.ts 的提示会把人引去查 nginx）。
#[tokio::test]
async fn marks_batch_accepts_over_default_body_limit() {
    let server = TestServer::start().await;
    let book_id = server.seed_shelf_book("https://up.example.com").await;

    // 每条约 1.2KB（中文 3 字节/字）：2000 条 ≈ 2.4MB
    let marks: Vec<Value> = (0..2000)
        .map(|index| {
            let mut mark = server.mark(&book_id, index % 10, index % 10_000, "underline");
            mark["bookText"] = json!(format!("第 {index} 条选文：{}", "选文内容".repeat(60)));
            mark["content"] = json!(format!("备注 {}", "备注内容".repeat(40)));
            mark
        })
        .collect();
    let payload = json!(marks);
    assert!(
        serde_json::to_vec(&payload).unwrap().len() > 1024 * 1024,
        "用例必须超过 /v1 默认 1MB，否则锁不住路由级体限"
    );

    let resp = server.post_json("/v1/me/marks:batch", payload).await;
    assert_eq!(resp.status(), StatusCode::OK, "路由级 8MB 体限应生效");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["saved"], json!(2000));
    assert_eq!(body["skipped"], json!(0));

    // 超过声明上限（5001 条，最小字段）：400 而不是静默截断
    let many: Vec<Value> = (0..5001)
        .map(|index| {
            json!({
                "bookId": book_id,
                "chapterIndex": index % 100,
                "chapterPos": index,
                "kind": "bookmark"
            })
        })
        .collect();
    let resp = server.post_json("/v1/me/marks:batch", json!(many)).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("MALFORMED_REQUEST"));

    server.cleanup();
}
