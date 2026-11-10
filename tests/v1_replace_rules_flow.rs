//! `/v1/me/replace-rules` 的集成测试：CRUD 往返、按名 upsert 的 201/200、
//! merge-patch、幂等删除、校验 422、批量写删。

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
            std::env::temp_dir().join(format!("reader-rust-v1-replace-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = common::base_config(&temp_dir, "v1-replace-test-secret");
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

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

#[tokio::test]
async fn replace_rules_require_auth_with_problem_401() {
    let server = TestServer::start().await;
    let resp = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");
    server.cleanup();
}

#[tokio::test]
async fn replace_rules_crud_roundtrip() {
    let server = TestServer::start().await;

    // 新建：201 + 条目
    let created = server
        .client
        .post(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .json(&json!({
            "name": "去广告",
            "pattern": "广告.*",
            "replacement": "",
            "isRegex": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let body: Value = created.json().await.unwrap();
    assert_eq!(body["name"], json!("去广告"));
    assert_eq!(body["isRegex"], json!(true));
    assert_eq!(body["isEnabled"], json!(true), "缺省启用");

    // 同名再建：200 整体替换
    let replaced = server
        .client
        .post(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .json(&json!({
            "name": "去广告",
            "pattern": "广告词.*",
            "replacement": "[已移除]",
            "isRegex": false,
            "scope": "book:1",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(replaced.status(), StatusCode::OK);

    // 列表：一条，字段完整
    let list: Value = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["pattern"], json!("广告词.*"));
    assert_eq!(list[0]["replacement"], json!("[已移除]"));
    assert_eq!(list[0]["scope"], json!("book:1"));

    // merge-patch：只改 pattern
    let patched = server
        .client
        .patch(server.url("/v1/me/replace-rules/%E5%8E%BB%E5%B9%BF%E5%91%8A"))
        .bearer_auth(&server.token)
        .json(&json!({ "pattern": "新广告.*" }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let body: Value = patched.json().await.unwrap();
    assert_eq!(body["pattern"], json!("新广告.*"));
    assert_eq!(body["replacement"], json!("[已移除]"), "未传字段保持不变");

    // 删除幂等：204 两次
    for _ in 0..2 {
        let deleted = server
            .client
            .delete(server.url("/v1/me/replace-rules/%E5%8E%BB%E5%B9%BF%E5%91%8A"))
            .bearer_auth(&server.token)
            .send()
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    }
    let list: Value = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.as_array().unwrap().is_empty());

    // PATCH 不存在的规则：404
    let missing = server
        .client
        .patch(server.url("/v1/me/replace-rules/%E4%B8%8D%E5%AD%98%E5%9C%A8"))
        .bearer_auth(&server.token)
        .json(&json!({ "pattern": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // 空 pattern：422 VALIDATION_FAILED 带 pointer
    let invalid = server
        .client
        .post(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .json(&json!({ "name": "坏规则", "pattern": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = invalid.json().await.unwrap();
    assert_eq!(body["code"], json!("VALIDATION_FAILED"));
    assert_eq!(
        body["errors"][0]["pointer"],
        json!("/pattern"),
        "空匹配串指向 /pattern"
    );

    server.cleanup();
}

#[tokio::test]
async fn batch_upsert_and_delete() {
    let server = TestServer::start().await;

    let batch = server
        .client
        .post(server.url("/v1/me/replace-rules:batch"))
        .bearer_auth(&server.token)
        .json(&json!({
            "rules": [
                { "name": "甲", "pattern": "a" },
                { "name": "乙", "pattern": "b", "isRegex": true }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch.status(), StatusCode::OK);
    let body: Value = batch.json().await.unwrap();
    assert_eq!(body["count"], json!(2));

    let list: Value = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);

    let deleted = server
        .client
        .post(server.url("/v1/me/replace-rules:batchDelete"))
        .bearer_auth(&server.token)
        .json(&json!({ "names": ["甲", "乙", "不存在"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let body: Value = deleted.json().await.unwrap();
    assert_eq!(body["deleted"], json!(3));

    let list: Value = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.as_array().unwrap().is_empty());

    server.cleanup();
}

#[tokio::test]
async fn rule_name_with_url_unsafe_chars_is_rejected() {
    let server = TestServer::start().await;

    for name in ["章节/广告", "带%号", "问号?", "井号#"] {
        let resp = server
            .client
            .post(server.url("/v1/me/replace-rules"))
            .bearer_auth(&server.token)
            .json(&json!({ "name": name, "pattern": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{name}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["code"], json!("VALIDATION_FAILED"));
        assert_eq!(body["errors"][0]["pointer"], json!("/name"));
    }

    // 批量 upsert 同样拒绝（否则会创建出路径打不中的规则）
    let batch = server
        .client
        .post(server.url("/v1/me/replace-rules:batch"))
        .bearer_auth(&server.token)
        .json(&json!({ "rules": [{ "name": "章节/广告", "pattern": "x" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch.status(), StatusCode::UNPROCESSABLE_ENTITY);

    server.cleanup();
}

#[tokio::test]
async fn patch_can_clear_nullable_fields() {
    let server = TestServer::start().await;

    // 建一条 book 域规则（scope = "book:1"）
    server
        .client
        .post(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .json(&json!({
            "name": "局部规则",
            "pattern": "x",
            "scope": "book:1",
            "group": "临时组",
            "replacement": "替换",
        }))
        .send()
        .await
        .unwrap();

    // {"scope": null} 清空 scope；{"replacement": null} 清空为默认空串；
    // 未传的 group 保持不变
    let patched = server
        .client
        .patch(server.url("/v1/me/replace-rules/局部规则"))
        .bearer_auth(&server.token)
        .json(&json!({ "scope": null, "replacement": null }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let body: Value = patched.json().await.unwrap();
    assert!(body["scope"].is_null(), "null 应清空 scope: {body}");
    assert_eq!(body["replacement"], json!(""), "null 应清空 replacement");
    assert_eq!(body["group"], json!("临时组"), "未传字段保持不变");

    // 字段缺省 = 不改：再 PATCH 只改 pattern，scope 保持 null
    let again = server
        .client
        .patch(server.url("/v1/me/replace-rules/局部规则"))
        .bearer_auth(&server.token)
        .json(&json!({ "pattern": "y" }))
        .send()
        .await
        .unwrap();
    let body: Value = again.json().await.unwrap();
    assert!(body["scope"].is_null());
    assert_eq!(body["pattern"], json!("y"));

    server.cleanup();
}

#[tokio::test]
async fn patch_isolates_fields_and_reports_precise_404() {
    let server = TestServer::start().await;

    server
        .client
        .post(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .json(&json!({ "name": "字段隔离", "pattern": "a", "replacement": "A", "order": 5 }))
        .send()
        .await
        .unwrap();

    // 只改 order：pattern/replacement 不被覆盖（原子读-改-写）
    let body: Value = server
        .client
        .patch(server.url("/v1/me/replace-rules/字段隔离"))
        .bearer_auth(&server.token)
        .json(&json!({ "order": 9 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["pattern"], json!("a"));
    assert_eq!(body["replacement"], json!("A"));
    assert_eq!(body["order"], json!(9));

    // 不存在：精确码 REPLACE_RULE_NOT_FOUND
    let missing = server
        .client
        .patch(server.url("/v1/me/replace-rules/不存在"))
        .bearer_auth(&server.token)
        .json(&json!({ "order": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let body: Value = missing.json().await.unwrap();
    assert_eq!(body["code"], json!("REPLACE_RULE_NOT_FOUND"));

    server.cleanup();
}

#[tokio::test]
async fn batch_rules_are_strict_and_capped() {
    let server = TestServer::start().await;

    // 未知字段：strict（与单条请求一致）
    let unknown = server
        .client
        .post(server.url("/v1/me/replace-rules:batch"))
        .bearer_auth(&server.token)
        .json(&json!({ "rules": [{ "name": "甲", "pattern": "a", "extra": 1 }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = unknown.json().await.unwrap();
    assert_eq!(body["code"], json!("UNKNOWN_FIELD"));

    // 超上限（101 条）→ 400
    let many: Vec<Value> = (0..101)
        .map(|i| json!({ "name": format!("规则{i}"), "pattern": "x" }))
        .collect();
    let over = server
        .client
        .post(server.url("/v1/me/replace-rules:batch"))
        .bearer_auth(&server.token)
        .json(&json!({ "rules": many }))
        .send()
        .await
        .unwrap();
    assert_eq!(over.status(), StatusCode::BAD_REQUEST);

    // 空列表 → 400
    let empty = server
        .client
        .post(server.url("/v1/me/replace-rules:batchDelete"))
        .bearer_auth(&server.token)
        .json(&json!({ "names": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    server.cleanup();
}

#[tokio::test]
async fn batch_delete_cleans_up_legacy_names_with_slashes() {
    let server = TestServer::start().await;

    // 模拟 /reader3 时代导入的遗留规则：名字含 /，路径接口打不中，
    // 但批量删除（名字在 body 里）可以清理
    sqlx::query(
        "INSERT INTO replace_rules (user_ns, name, id, pattern, replacement, is_enabled, is_regex, rule_order)          VALUES ('user_1', '章节/广告', 0, 'x', '', 1, 0, 0)",
    )
    .execute(&server.pool)
    .await
    .unwrap();

    let deleted = server
        .client
        .post(server.url("/v1/me/replace-rules:batchDelete"))
        .bearer_auth(&server.token)
        .json(&json!({ "names": ["章节/广告"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);

    let list: Value = server
        .client
        .get(server.url("/v1/me/replace-rules"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        list.as_array().unwrap().is_empty(),
        "遗留规则应被批量删除清掉"
    );

    server.cleanup();
}
