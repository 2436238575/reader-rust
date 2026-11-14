//! AI 代理路由的入站超时分组：非流式生成（响应头要等上游**生成完**才返回）
//! 不能被常规接口的 `INBOUND_REQUEST_TIMEOUT_SECS` 掐死——aiProxy 与上传
//! 路由同档使用 `INBOUND_UPLOAD_TIMEOUT_SECS`，上游调用自身的预算见
//! `model/ai_proxy.rs`（300s reqwest 客户端超时）。

mod common;
use axum::{routing::post, Json, Router};
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use std::path::PathBuf;

const TEST_USERNAME: &str = "reader1";
const TEST_PASSWORD: &str = "password123";

/// 假模型上游：等 `delay` 再回 JSON——复现非流式生成「响应头要等生成完」
/// 的形态（流式调用会立即回响应头、生成在 body 里继续，不受此影响）。
async fn start_slow_upstream(delay: std::time::Duration) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            tokio::time::sleep(delay).await;
            Json(serde_json::json!({
                "choices": [{ "message": { "content": "ok" } }]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// 假模型上游：固定返回错误状态与 JSON 错误体。
async fn start_error_upstream(status: axum::http::StatusCode, message: &'static str) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            (
                status,
                Json(serde_json::json!({ "error": { "message": message } })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

impl TestServer {
    /// 常规接口的入站超时缩短到 2s：aiProxy 若仍挂在常规组里，
    /// 3.5s 的上游生成会以 408 收场。（上游地址由客户端在请求体里自带，
    /// 服务端配置不涉及。）
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-aiproxy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let web_root = temp_dir.join("dist");
        std::fs::create_dir_all(&web_root).unwrap();
        std::fs::write(
            web_root.join("index.html"),
            "<!doctype html><title>app</title>",
        )
        .unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: web_root.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "ai-proxy-flow-secret".to_string(),
            admin_username: TEST_USERNAME.to_string(),
            admin_password: TEST_PASSWORD.to_string(),
            rate_limit_disabled: true,
            // 假上游监听 127.0.0.1：显式放行私网出站（默认拦截）
            allow_private_network: true,
            inbound_request_timeout_secs: 2,
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

        Self {
            base_url: format!("http://{addr}"),
            temp_dir,
            client: reqwest::Client::new(),
        }
    }

    async fn login_token(&self) -> String {
        let res = self
            .client
            .post(format!("{}/v1/auth/login", self.base_url))
            .header("x-requested-with", "XMLHttpRequest")
            .json(&serde_json::json!({
                "username": TEST_USERNAME,
                "password": TEST_PASSWORD,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), reqwest::StatusCode::OK, "登录应成功");
        let body: serde_json::Value = res.json().await.unwrap();
        body["accessToken"]
            .as_str()
            .expect("登录响应应含 accessToken")
            .to_string()
    }
}

/// 超过常规入站超时（2s）的非流式生成必须等满并透传上游响应：
/// aiProxy/aiProxyImage 与上传路由同档使用 `INBOUND_UPLOAD_TIMEOUT_SECS`
/// （默认 900s），不在 120s 常规组里。挂错组时本用例会以 408
/// 「请求超时」失败。
#[tokio::test]
async fn slow_non_streaming_generation_survives_inbound_timeout() {
    let upstream = start_slow_upstream(std::time::Duration::from_millis(3500)).await;
    let app = TestServer::start().await;
    let token = app.login_token().await;

    let res = app
        .client
        .post(format!("{}/reader3/aiProxy", app.base_url))
        .header("Cookie", format!("reader_access={token}"))
        .json(&serde_json::json!({
            "baseUrl": upstream,
            "path": "/v1/chat/completions",
            "body": { "messages": [{ "role": "user", "content": "hi" }] },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        reqwest::StatusCode::OK,
        "3.5s 的非流式生成不应被 2s 的常规入站超时掐死（应等满并透传）"
    );
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"], "ok",
        "应透传上游 JSON 响应"
    );

    // v1 入口与 legacy 同档：同样不被 2s 常规入站超时掐死
    let res = app
        .client
        .post(format!("{}/v1/ai/proxy", app.base_url))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({
            "baseUrl": upstream,
            "path": "/v1/chat/completions",
            "body": { "messages": [{ "role": "user", "content": "hi" }] },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::OK, "v1 代理也应等满");
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
}

#[tokio::test]
async fn v1_proxy_upstream_error_is_problem_json() {
    let upstream = start_error_upstream(axum::http::StatusCode::UNAUTHORIZED, "bad api key").await;
    let app = TestServer::start().await;
    let token = app.login_token().await;

    let res = app
        .client
        .post(format!("{}/v1/ai/proxy", app.base_url))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({
            "baseUrl": upstream,
            "path": "/v1/chat/completions",
            "body": { "messages": [] },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::BAD_GATEWAY);
    assert_eq!(
        res.headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json")
    );
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["code"], "UPSTREAM_FAILED", "{body}");
    assert_eq!(body["upstreamCode"], "401", "{body}");
    let detail = body["detail"].as_str().unwrap_or_default();
    assert!(detail.contains("401"), "detail 应带真实状态码：{detail}");
    assert!(
        detail.contains("bad api key"),
        "detail 应带上游原因：{detail}"
    );
}
