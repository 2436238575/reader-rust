//! 书源摘要列表端点（getBookSourceList）：
//! - 列表只回展示字段 + exploreUrl，规则 JSON 不下发（省流量的核心保证）；
//! - getBookSources（全量）与 getBookSource（单条全量）行为不变；
//! - 端点在需登录组，未认证请求拒绝。

mod common;

use axum::http::StatusCode;
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use serde_json::{json, Value};
use std::path::PathBuf;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

async fn boot() -> TestServer {
    common::clean_stale_temp_dirs();
    let temp_dir =
        std::env::temp_dir().join(format!("reader-rust-source-list-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let cfg = AppConfig {
        server_host: "127.0.0.1".to_string(),
        server_port: 0,
        database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
        storage_dir: temp_dir.to_string_lossy().to_string(),
        web_root: temp_dir.to_string_lossy().to_string(),
        assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
        jwt_secret: "source-list-secret".to_string(),
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
        common::remove_dir_retried(&self.temp_dir);
    }
}

impl TestServer {
    async fn get(&self, path: &str, authed: bool) -> (StatusCode, Value) {
        let mut req = self.client.get(format!("{}/reader3/{path}", self.base_url));
        if authed {
            req = req.bearer_auth(&self.token);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status();
        let payload: Value = resp.json().await.unwrap();
        (status, payload)
    }

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
        resp.json().await.unwrap()
    }
}

#[tokio::test]
async fn source_list_returns_summary_without_rule_payload() {
    let server = boot().await;
    let saved = server
        .post(
            "saveBookSources",
            json!([
                {
                    "bookSourceName": "规则源",
                    "bookSourceUrl": "https://summary-a.example",
                    "bookSourceGroup": "小说",
                    "enabled": true,
                    "enabledExplore": true,
                    "exploreUrl": "榜单::/explore",
                    "searchUrl": "/search?q={{key}}",
                    "header": "{\"User-Agent\": \"summary-test\"}",
                    "ruleContent": { "content": "$.data.content" },
                    "ruleSearch": { "bookList": ".item" }
                },
                {
                    "bookSourceName": "干净源",
                    "bookSourceUrl": "https://summary-b.example",
                    "enabled": false
                }
            ]),
        )
        .await;
    assert_eq!(saved["data"]["count"], 2);

    // 未认证请求拒绝：端点挂在需登录组
    let (status, _) = server.get("getBookSourceList", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (_, payload) = server.get("getBookSourceList", true).await;
    let list = payload["data"].as_array().expect("data 应为数组");
    assert_eq!(list.len(), 2);

    let a = list
        .iter()
        .find(|s| s["bookSourceUrl"] == "https://summary-a.example")
        .unwrap();
    assert!(a["bookSourceId"].is_i64(), "摘要必须携带身份键 bookSourceId");
    assert_eq!(a["bookSourceName"], "规则源");
    assert_eq!(a["bookSourceGroup"], "小说");
    assert_eq!(a["enabled"], true);
    assert_eq!(a["enabledExplore"], true);
    assert_eq!(a["exploreUrl"], "榜单::/explore", "发现页解析分类必需");
    // 省流量的核心断言：规则/抓取字段一律不下发
    for forbidden in [
        "ruleContent",
        "ruleSearch",
        "ruleBookInfo",
        "ruleToc",
        "ruleExplore",
        "searchUrl",
        "header",
        "jsLib",
        "loginUrl",
    ] {
        assert!(a.get(forbidden).is_none(), "摘要不应携带 {forbidden}");
    }

    // 全量接口行为不变：规则仍在
    let (_, full_payload) = server.get("getBookSources", true).await;
    let full = full_payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["bookSourceUrl"] == "https://summary-a.example")
        .unwrap();
    assert_eq!(full["ruleContent"]["content"], "$.data.content");

    // 单条全量接口行为不变：按 id 取回完整规则
    let id = a["bookSourceId"].as_i64().unwrap();
    let (_, one) = server
        .get(&format!("getBookSource?bookSourceId={id}"), true)
        .await;
    assert_eq!(one["data"]["bookSourceName"], "规则源");
    assert_eq!(one["data"]["ruleContent"]["content"], "$.data.content");
    assert_eq!(one["data"]["exploreUrl"], "榜单::/explore");
}
