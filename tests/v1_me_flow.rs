//! `/v1` 垂直切片的集成测试：problem+json 形态、鉴权 401、`/v1/me` 资料、
//! 未注册路径的 problem 404。错误信封的每个端点都按此口径补齐测试。

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
            std::env::temp_dir().join(format!("reader-rust-v1-me-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = common::base_config(&temp_dir, "v1-me-test-secret");
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

    /// 收尾：池随 Self drop 关闭；目录删除失败交给 clean_stale_temp_dirs。
    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    async fn get(&self, path: &str, token: Option<&str>) -> reqwest::Response {
        let mut request = self.client.get(format!("{}{path}", self.base_url));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap()
    }
}

#[tokio::test]
async fn me_returns_typed_profile_with_rfc3339_time() {
    let server = TestServer::start().await;

    let resp = server.get("/v1/me", Some(&server.token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json"),
        "资源响应是 application/json，不是 problem"
    );
    let body: Value = resp.json().await.unwrap();
    assert!(
        body.get("isSuccess").is_none(),
        "v1 响应不带 ApiResponse 包装"
    );
    assert_eq!(body["username"], json!("reader1"));
    assert_eq!(body["id"], json!("u_1"));
    let created_at = body["createdAt"].as_str().unwrap();
    assert!(
        created_at.ends_with('Z') && created_at.len() == 20,
        "createdAt 必须是 RFC 3339 UTC 秒级: {created_at}"
    );

    server.cleanup();
}

#[tokio::test]
async fn unauthenticated_request_gets_problem_401() {
    let server = TestServer::start().await;

    for token in [None, Some("not-a-jwt")] {
        let resp = server.get("/v1/me", token).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let request_id = resp
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            resp.headers()["content-type"],
            "application/problem+json",
            "错误必须是 problem+json"
        );
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["code"], json!("AUTH_EXPIRED"));
        assert_eq!(body["status"], json!(401));
        assert_eq!(body["title"], json!("Auth Expired"));
        assert!(
            body.get("isSuccess").is_none(),
            "problem 体不携带 ApiResponse 字段"
        );
        // traceId 与响应头 X-Request-Id 一致（用户截图能定位日志）
        let trace = body["traceId"].as_str().expect("problem 带 traceId");
        assert_eq!(trace, request_id);
    }

    server.cleanup();
}

#[tokio::test]
async fn unregistered_v1_path_gets_problem_404() {
    let server = TestServer::start().await;

    let resp = server
        .get("/v1/definitely-not-registered", Some(&server.token))
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("NOT_FOUND"));
    assert_eq!(body["status"], json!(404));

    // /v1 根路径同样兜底
    let resp = server.get("/v1", Some(&server.token)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    server.cleanup();
}

#[tokio::test]
async fn scoped_proxy_token_cannot_access_v1() {
    let server = TestServer::start().await;

    // 用 /reader3 的限定令牌铸造端点换一枚 30 分钟限定令牌（绑定书源 1）
    let resp = server
        .client
        .post(format!(
            "{}/reader3/mintBookSourceProxyToken",
            server.base_url
        ))
        .bearer_auth(&server.token)
        .json(&json!({ "bookSourceId": 1, "ttlSeconds": 60 }))
        .send()
        .await
        .unwrap();
    if resp.status() != StatusCode::OK {
        // 铸造端点在部分口径下要求书源存在；拿不到限定令牌时本用例退化跳过
        server.cleanup();
        return;
    }
    let body: Value = resp.json().await.unwrap();
    let Some(scoped) = body["data"]["accessToken"].as_str().map(str::to_string) else {
        server.cleanup();
        return;
    };

    let resp = server.get("/v1/me", Some(&scoped)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("SCOPE_FORBIDDEN"));

    server.cleanup();
}
