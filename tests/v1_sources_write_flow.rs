//! `/v1/sources` 写链路（同步子集）：单条 upsert（201/200）、详情、批量
//! upsert、删除/批量删除/清空（confirm 门槛）/删失效。

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
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-src-write-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = common::base_config(&temp_dir, "v1-src-write-test-secret");
        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let (base_url, _server) = common::spawn_app(app).await;
        let client = reqwest::Client::new();
        let token = common::login(&client, &base_url).await;
        Self {
            base_url,
            temp_dir,
            client,
            token,
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
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

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
    }

    async fn delete_json(&self, path: &str, body: Value) -> reqwest::Response {
        self.client
            .delete(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
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

fn source_json(url: &str, name: &str) -> Value {
    json!({
        "bookSourceName": name,
        "bookSourceUrl": url,
        "enabled": true,
        "ruleSearch": { "bookList": "$.data.list[*]" }
    })
}

#[tokio::test]
async fn upsert_get_and_delete_roundtrip() {
    let server = TestServer::start().await;

    // 新建：201 + src_<n>
    let resp = server
        .post_json("/v1/sources", source_json("https://a.example.com", "甲"))
        .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = resp.json().await.unwrap();
    let id = body["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("src_"));

    // 同 URL 再提交：200（按 URL 保号替换）
    let resp = server
        .post_json("/v1/sources", source_json("https://a.example.com", "甲改"))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"], json!(id), "URL 去重保号");

    // 详情：完整源材料（含规则），线缆字段之外原样透传
    let resp = server.get(&format!("/v1/sources/{id}")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["bookSourceName"], json!("甲改"));
    assert_eq!(body["bookSourceUrl"], json!("https://a.example.com"));
    assert_eq!(body["ruleSearch"]["bookList"], json!("$.data.list[*]"));

    // 自带 id 改 URL：走 update 分支，也算替换（200）而不是 201 新建
    let numeric_id = id.trim_start_matches("src_").parse::<i64>().unwrap();
    let resp = server
        .post_json(
            "/v1/sources",
            json!({
                "bookSourceId": numeric_id,
                "bookSourceName": "甲改并迁移地址",
                "bookSourceUrl": "https://a-moved.example.com"
            }),
        )
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "id 命中的更新不能误报 201 新建"
    );
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"], json!(id), "改 URL 保号");
    let body: Value = server
        .get(&format!("/v1/sources/{id}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["bookSourceUrl"], json!("https://a-moved.example.com"));

    // 缺 bookSourceUrl：400
    let resp = server
        .post_json("/v1/sources", json!({ "bookSourceName": "无地址" }))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("MALFORMED_REQUEST"));

    // 未知书源：404 精确码
    let resp = server.get("/v1/sources/src_999999").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("SOURCE_NOT_FOUND"));

    // 删除：204 + 幂等；详情 404
    let resp = server
        .client
        .delete(format!("{}/v1/sources/{id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = server
        .client
        .delete(format!("{}/v1/sources/{id}", server.base_url))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = server.get(&format!("/v1/sources/{id}")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    server.cleanup();
}

#[tokio::test]
async fn upsert_accepts_legacy_fields_and_ignores_unknown() {
    let server = TestServer::start().await;

    // legado 旧字段名迁移 + 未知附加字段忽略
    let resp = server
        .post_json(
            "/v1/sources",
            json!({
                "bookSourceName": "旧字段源",
                "bookSourceUrl": "https://legacy.example.com",
                "enable": false,
                "serialNumber": 7,
                "enableComment": true
            }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "未知字段应被忽略");
    let body: Value = resp.json().await.unwrap();
    let id = body["id"].as_str().unwrap().to_string();

    let body: Value = server
        .get(&format!("/v1/sources/{id}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["enabled"], json!(false), "enable → enabled 迁移");
    assert_eq!(
        body["customOrder"],
        json!(7),
        "serialNumber → customOrder 迁移"
    );

    server.cleanup();
}

#[tokio::test]
async fn batch_upsert_deletes_and_delete_all() {
    let server = TestServer::start().await;

    // 批量 upsert：2 条
    let resp = server
        .post_json(
            "/v1/sources:batchUpsert",
            json!({ "sources": [
                source_json("https://b1.example.com", "乙一"),
                source_json("https://b2.example.com", "乙二")
            ] }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["count"], json!(2));

    // 裸数组不是合法请求体（契约是 {sources}）：结构错误 → 400 MALFORMED_REQUEST
    let resp = server
        .post_json(
            "/v1/sources:batchUpsert",
            json!([source_json("https://x.example.com", "X")]),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("MALFORMED_REQUEST"));

    // 空列表 → 400
    let resp = server
        .post_json("/v1/sources:batchUpsert", json!({ "sources": [] }))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 请求体超过 /v1 默认 1MB：路由级 64MB 必须生效（内层 layer 覆盖外层）。
    // 这里没有别的机制兜底，层序一旦被改会静默变回 1MB
    let big_source = json!({
        "bookSourceName": "大源",
        "bookSourceUrl": "https://big.example.com",
        "jsLib": "a".repeat(2_000_000)
    });
    let payload = json!({ "sources": [big_source] });
    assert!(
        serde_json::to_vec(&payload).unwrap().len() > 1024 * 1024,
        "用例必须超过 /v1 默认 1MB，否则锁不住路由级体限"
    );
    let resp = server.post_json("/v1/sources:batchUpsert", payload).await;
    assert_eq!(resp.status(), StatusCode::OK, "路由级 64MB 体限应生效");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["count"], json!(1));

    let list: Value = server
        .get("/v1/sources?limit=200")
        .await
        .json()
        .await
        .unwrap();
    let ids: Vec<String> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 3, "两条正常导入 + 一条 2MB 大源");

    // 批量删除：存在的 1 个 + 不存在的 1 个 → deleted 1
    let resp = server
        .post_json(
            "/v1/sources:batchDelete",
            json!({ "ids": [ids[0], "src_999999"] }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["deleted"], json!(1));

    // confirm 门槛
    let resp = server
        .delete_json("/v1/sources", json!({ "confirm": "确认" }))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = server
        .delete_json("/v1/sources", json!({ "confirm": "DELETE_ALL" }))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["deleted"], json!(2), "批量删除 1 条后剩 2 条");

    let list: Value = server.get("/v1/sources").await.json().await.unwrap();
    assert_eq!(list["total"], json!(0));

    server.cleanup();
}

#[tokio::test]
async fn delete_invalid_removes_invalid_group() {
    let server = TestServer::start().await;

    server
        .post_json("/v1/sources", source_json("https://ok.example.com", "正常"))
        .await;
    server
        .post_json(
            "/v1/sources",
            json!({
                "bookSourceName": "坏源",
                "bookSourceUrl": "https://bad.example.com",
                "bookSourceGroup": "默认,失效"
            }),
        )
        .await;

    let resp = server
        .post_json("/v1/sources:deleteInvalid", json!({}))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["deleted"], json!(1));

    let list: Value = server.get("/v1/sources").await.json().await.unwrap();
    let names: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["正常"]);

    server.cleanup();
}
