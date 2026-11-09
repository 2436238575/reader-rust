//! `/v1/auth` 域的集成测试：Cookie 语义（Lax / Path）、令牌轮换与重放
//! 吊销、登录失败统一 401、限速 429、CSRF 头、改密吊销其他会话。

use axum::http::{header, StatusCode};
use serde_json::{json, Value};
use std::path::PathBuf;

use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;

mod common;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    /// 独立连到测试库的池：回拨 refresh 轮换时间戳（穿过 30s 宽限窗口）用。
    pool: sqlx::SqlitePool,
    /// Router 的克隆：body 上限类用例走进程内 oneshot，绕开 TCP——真实
    /// socket 上「服务端读完上限就停止读取并关闭」会与客户端继续写形成 RST
    /// 竞态，413 与连接重置的胜负取决于时序（约三分之一翻车）。
    app: axum::Router,
}

impl TestServer {
    async fn start() -> Self {
        Self::start_with(|cfg| cfg).await
    }

    /// 允许单测调整配置（如打开登录限速）。
    async fn start_with(mutate: impl FnOnce(AppConfig) -> AppConfig) -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-v1-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = mutate(common::base_config(&temp_dir, "v1-auth-test-secret"));
        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let (base_url, _server) = common::spawn_app(app.clone()).await;
        let pool = sqlx::SqlitePool::connect(&format!(
            "sqlite:{}?mode=rw",
            temp_dir.join("reader.db").display()
        ))
        .await
        .unwrap();
        Self {
            base_url,
            temp_dir,
            client: reqwest::Client::builder()
                // 登录响应的 Set-Cookie 需要被客户端保存（refresh 轮换用例依赖）
                .cookie_store(true)
                .build()
                .unwrap(),
            pool,
            app,
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }

    async fn post_json(&self, path: &str, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}{path}", self.base_url))
            .header("x-requested-with", "XMLHttpRequest")
            .json(&body)
            .send()
            .await
            .unwrap()
    }
}

/// 手工携带 Cookie 的请求（不走 cookie_store，精确控制轮换与重放场景）。
struct RawClient {
    base_url: String,
    client: reqwest::Client,
}

impl RawClient {
    fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            client: reqwest::Client::new(),
        }
    }

    async fn post(&self, path: &str, cookies: &[(String, String)]) -> reqwest::Response {
        let mut request = self
            .client
            .post(format!("{}{path}", self.base_url))
            .header("x-requested-with", "XMLHttpRequest");
        for (name, value) in cookies {
            request = request.header(header::COOKIE, format!("{name}={value}"));
        }
        request.send().await.unwrap()
    }

    async fn post_bearer(&self, path: &str, token: &str, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }
}

fn cookie_pairs(response: &reqwest::Response) -> Vec<(String, String)> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|raw| {
            let (pair, _) = raw.split_once(';')?;
            let (name, value) = pair.split_once('=')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn cookie_attribute(response: &reqwest::Response, name: &str) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|raw| raw.starts_with(&format!("{name}=")))
        .expect("cookie should be present")
        .to_string()
}

#[tokio::test]
async fn login_issues_cookies_with_v1_semantics_and_token_pair() {
    let server = TestServer::start().await;

    let resp = server
        .post_json(
            "/v1/auth/login",
            json!({"username": "reader1", "password": "password123"}),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/json"));

    // Cookie 语义：HttpOnly + SameSite=Lax；access Path=/（覆盖 /reader3
    // 过渡期业务接口）；refresh 收紧到 /v1/auth
    let access = cookie_attribute(&resp, "reader_access");
    let refresh = cookie_attribute(&resp, "reader_refresh");
    assert!(access.contains("HttpOnly"), "{access}");
    assert!(access.contains("SameSite=Lax"), "{access}");
    assert!(access.contains("Path=/;"), "{access}");
    assert!(refresh.contains("Path=/v1/auth;"), "{refresh}");

    // 响应体：令牌对；refreshToken 永不出现
    let body: Value = resp.json().await.unwrap();
    assert!(body["accessToken"].as_str().unwrap().contains('.'));
    let expires_at = body["accessTokenExpiresAt"].as_str().unwrap();
    assert!(
        expires_at.ends_with('Z') && expires_at.len() == 20,
        "{expires_at}"
    );
    assert!(
        body.get("refreshToken").is_none(),
        "refreshToken 不进响应体"
    );

    server.cleanup();
}

#[tokio::test]
async fn login_failure_is_uniform_401_invalid_credentials() {
    let server = TestServer::start().await;

    for creds in [
        json!({"username": "reader1", "password": "wrong"}),
        json!({"username": "ghost", "password": "whatever"}),
    ] {
        let resp = server.post_json("/v1/auth/login", creds).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp.headers()["content-type"], "application/problem+json");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["code"], json!("INVALID_CREDENTIALS"));
        assert_eq!(body["detail"], json!("用户名或密码错误"));
    }

    server.cleanup();
}

#[tokio::test]
async fn refresh_rotates_and_replay_revokes_the_family() {
    let server = TestServer::start().await;
    let raw = RawClient::new(&server.base_url);

    // 第一次登录拿一对令牌
    let login = server
        .post_json(
            "/v1/auth/login",
            json!({"username": "reader1", "password": "password123"}),
        )
        .await;
    let pairs = cookie_pairs(&login);
    let refresh_v1 = pairs
        .iter()
        .find(|(name, _)| name == "reader_refresh")
        .expect("登录应下发 refresh Cookie")
        .clone();

    // 轮换：旧 refresh 换新对，两次下发值必须不同
    let rotated = raw
        .post("/v1/auth/refresh", std::slice::from_ref(&refresh_v1))
        .await;
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated_pairs = cookie_pairs(&rotated);
    let rotated_body: Value = rotated.json().await.unwrap();
    assert!(rotated_body["accessToken"].as_str().is_some());
    let new_pairs = rotated_pairs;
    let refresh_v2 = new_pairs
        .iter()
        .find(|(name, _)| name == "reader_refresh")
        .expect("轮换应下发新 refresh Cookie")
        .clone();
    assert_ne!(refresh_v1.1, refresh_v2.1, "一次性轮换：值必须变化");

    // 回拨旧令牌的 used_at，穿过 30s 重放宽限窗口（窗口内的重放是容错：
    // 断网重试/双标签页并发），让重放真正命中吊销分支
    sqlx::query(
        "UPDATE refresh_tokens SET used_at = used_at - 60 WHERE uid = 1 AND used_at IS NOT NULL",
    )
    .execute(&server.pool)
    .await
    .unwrap();

    // 旧 refresh 重放：会话家族吊销，401 AUTH_REFRESH_REPLAYED
    let replay = raw.post("/v1/auth/refresh", &[refresh_v1]).await;
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    let cleared = cookie_pairs(&replay);
    assert_eq!(replay.headers()["content-type"], "application/problem+json");
    let body: Value = replay.json().await.unwrap();
    assert_eq!(body["code"], json!("AUTH_REFRESH_REPLAYED"));
    assert!(
        cleared
            .iter()
            .any(|(name, value)| name == "reader_refresh" && value.is_empty()),
        "失败响应应清除 Cookie"
    );

    // 家族已吊销：新令牌同样失效
    let after_revoke = raw.post("/v1/auth/refresh", &[refresh_v2]).await;
    assert_eq!(after_revoke.status(), StatusCode::UNAUTHORIZED);

    server.cleanup();
}

#[tokio::test]
async fn refresh_without_cookie_gets_401_with_cleared_cookies() {
    let server = TestServer::start().await;
    let raw = RawClient::new(&server.base_url);

    let resp = raw.post("/v1/auth/refresh", &[]).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let cleared = cookie_pairs(&resp);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("AUTH_EXPIRED"));
    assert_eq!(cleared.len(), 2, "access 与 refresh 都要清");

    server.cleanup();
}

#[tokio::test]
async fn logout_revokes_family_and_clears_cookies() {
    let server = TestServer::start().await;
    let raw = RawClient::new(&server.base_url);

    let login = server
        .post_json(
            "/v1/auth/login",
            json!({"username": "reader1", "password": "password123"}),
        )
        .await;
    let pairs = cookie_pairs(&login);
    let refresh = pairs
        .into_iter()
        .find(|(name, _)| name == "reader_refresh")
        .unwrap();

    let logout = raw
        .post("/v1/auth/logout", std::slice::from_ref(&refresh))
        .await;
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let cleared = cookie_pairs(&logout);
    assert!(
        cleared
            .iter()
            .any(|(name, value)| name == "reader_access" && value.is_empty()),
        "access Cookie 应清除"
    );
    assert!(
        cleared
            .iter()
            .any(|(name, value)| name == "reader_refresh" && value.is_empty()),
        "refresh Cookie 应清除"
    );

    // 家族已吊销：登出后的 refresh 不再可用
    let after = raw.post("/v1/auth/refresh", &[refresh]).await;
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);

    server.cleanup();
}

#[tokio::test]
async fn password_change_revokes_other_sessions_and_rotates_current() {
    let server = TestServer::start().await;
    let raw = RawClient::new(&server.base_url);

    // 两台「设备」各登录一次
    let device_a = server
        .post_json(
            "/v1/auth/login",
            json!({"username": "reader1", "password": "password123"}),
        )
        .await;
    let refresh_a = cookie_pairs(&device_a)
        .into_iter()
        .find(|(name, _)| name == "reader_refresh")
        .unwrap();
    let access_a: Value = device_a.json().await.unwrap();
    let token_a = access_a["accessToken"].as_str().unwrap().to_string();

    let device_b = server
        .post_json(
            "/v1/auth/login",
            json!({"username": "reader1", "password": "password123"}),
        )
        .await;
    let body_b: Value = device_b.json().await.unwrap();
    let token_b = body_b["accessToken"].as_str().unwrap().to_string();

    // 设备 A 改密：成功 + 换发新令牌对
    let change = raw
        .post_bearer(
            "/v1/auth/password",
            &token_a,
            json!({
                "currentPassword": "password123",
                "newPassword": "new-password-456",
            }),
        )
        .await;
    assert_eq!(change.status(), StatusCode::NO_CONTENT);
    assert!(
        cookie_attribute(&change, "reader_access").contains("Max-Age="),
        "当前设备换发新 Cookie"
    );

    // 设备 B 的 access token 立即失效（token_version 吊销）
    let probe_b = RawClient::new(&server.base_url)
        .post_bearer(
            "/v1/auth/password",
            &token_b,
            json!({
                "currentPassword": "new-password-456",
                "newPassword": "whatever-789",
            }),
        )
        .await;
    assert_eq!(probe_b.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        probe_b.headers()["content-type"],
        "application/problem+json"
    );

    // 旧 refresh 家族同样被吊销
    let old_refresh = raw.post("/v1/auth/refresh", &[refresh_a]).await;
    assert_eq!(old_refresh.status(), StatusCode::UNAUTHORIZED);

    // 新密码可登录，旧密码不可
    let old_login = server
        .post_json(
            "/v1/auth/login",
            json!({"username": "reader1", "password": "password123"}),
        )
        .await;
    assert_eq!(old_login.status(), StatusCode::UNAUTHORIZED);
    let new_login = server
        .post_json(
            "/v1/auth/login",
            json!({"username": "reader1", "password": "new-password-456"}),
        )
        .await;
    assert_eq!(new_login.status(), StatusCode::OK);

    server.cleanup();
}

#[tokio::test]
async fn non_get_without_csrf_header_is_rejected_with_403() {
    let server = TestServer::start().await;

    // 无 X-Requested-With、无 Bearer：403 CSRF_HEADER_MISSING
    let resp = server
        .client
        .post(format!("{}/v1/auth/login", server.base_url))
        .json(&json!({"username": "reader1", "password": "password123"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(resp.headers()["content-type"], "application/problem+json");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], json!("CSRF_HEADER_MISSING"));

    // 带头即放行
    let ok = server
        .client
        .post(format!("{}/v1/auth/login", server.base_url))
        .header("x-requested-with", "XMLHttpRequest")
        .json(&json!({"username": "reader1", "password": "password123"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    server.cleanup();
}

/// Bearer 客户端豁免 CSRF 头：改密只凭 Authorization 也应通过。
#[tokio::test]
async fn bearer_requests_are_exempt_from_csrf_header() {
    let server = TestServer::start().await;
    let raw = RawClient::new(&server.base_url);

    let login = server
        .post_json(
            "/v1/auth/login",
            json!({"username": "reader1", "password": "password123"}),
        )
        .await;
    let body: Value = login.json().await.unwrap();
    let token = body["accessToken"].as_str().unwrap().to_string();

    // 无 X-Requested-With、只有 Bearer：放行（Bearer 不经 Cookie，天然免疫 CSRF）
    let resp = raw
        .post_bearer(
            "/v1/auth/password",
            &token,
            json!({
                "currentPassword": "password123",
                "newPassword": "bearer-exempt-1",
            }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    server.cleanup();
}

/// 进程内驱动 Router（不经 TCP）：body 超限返回确定性的 413 响应体。
async fn oneshot_json(
    app: axum::Router,
    path: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    use tower::util::ServiceExt;
    let mut builder = axum::http::Request::builder()
        .method("POST")
        .uri(path)
        .header("x-requested-with", "XMLHttpRequest");
    if let Some(content_type) = content_type {
        builder = builder.header("content-type", content_type);
    }
    let mut request = builder.body(axum::body::Body::from(body)).unwrap();
    // 真实服务经 into_make_service_with_connect_info 注入连接信息；
    // 进程内驱动要手工补上，否则登录 handler 的 ConnectInfo 提取器直接 500
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            0,
        ))));
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, headers, value)
}

/// 公开认证端点按 1MB 上限缓冲（不是最外层 100MB 兜底）：2MB JSON 必须 413，
/// 且响应体是 problem+json 的 PAYLOAD_TOO_LARGE。进程内 oneshot 驱动，结果
/// 确定——真实 socket 的 RST 竞态会让 413 与连接重置随机决胜。
#[tokio::test]
async fn login_body_is_capped_at_1mb_with_problem_413() {
    let server = TestServer::start().await;

    let big_password = "x".repeat(2 * 1024 * 1024);
    let body =
        serde_json::to_vec(&json!({"username": "reader1", "password": big_password})).unwrap();
    let (status, headers, value) = oneshot_json(
        server.app.clone(),
        "/v1/auth/login",
        Some("application/json"),
        body,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "超过 1MB 上限应 413: {value}"
    );
    assert_eq!(
        headers["content-type"], "application/problem+json",
        "413 必须是 problem+json"
    );
    assert_eq!(value["code"], json!("PAYLOAD_TOO_LARGE"));

    // 正常小 body 不受影响（真实 HTTP 路径也验一次）
    let resp = server
        .post_json(
            "/v1/auth/login",
            json!({"username": "reader1", "password": "password123"}),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK, "小 body 不应被上限误伤");

    server.cleanup();
}

/// 缺 content-type：415 problem+json（axum 默认是纯文本 415）。
#[tokio::test]
async fn missing_content_type_gets_problem_415() {
    let server = TestServer::start().await;
    let body = serde_json::to_vec(&json!({"username": "reader1", "password": "x"})).unwrap();
    let (status, headers, value) =
        oneshot_json(server.app.clone(), "/v1/auth/login", None, body).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(headers["content-type"], "application/problem+json");
    assert_eq!(value["code"], json!("UNSUPPORTED_MEDIA_TYPE"));
    server.cleanup();
}

/// 非法 JSON：400 MALFORMED_REQUEST problem+json。
#[tokio::test]
async fn malformed_json_gets_problem_400() {
    let server = TestServer::start().await;
    let (status, headers, value) = oneshot_json(
        server.app.clone(),
        "/v1/auth/login",
        Some("application/json"),
        b"{not json".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(headers["content-type"], "application/problem+json");
    assert_eq!(value["code"], json!("MALFORMED_REQUEST"));
    server.cleanup();
}

#[tokio::test]
async fn login_rate_limit_returns_429_with_retry_after() {
    // 测试配置默认豁免限速（RATE_LIMIT_DISABLED=true），这条用例单独打开：
    // 连续错密码触发用户名维度的封禁（USER_LOGIN_MAX_FAILURES 次）
    let server = TestServer::start_with(|mut cfg| {
        cfg.rate_limit_disabled = false;
        cfg
    })
    .await;
    let raw = RawClient::new(&server.base_url);
    let mut seen_429 = false;
    for _ in 0..12 {
        let resp = raw
            .post_bearer(
                "/v1/auth/login",
                "",
                json!({"username": "bruteforce", "password": "wrong"}),
            )
            .await;
        // 空 bearer 会触发 CSRF？不会：bearer_auth("") 仍写 Authorization 头 → 豁免。
        // 但登录失败的形态是 401 或 429
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(resp.headers()["content-type"], "application/problem+json");
            assert!(
                resp.headers().get(header::RETRY_AFTER).is_some(),
                "429 必须带 Retry-After"
            );
            let body: Value = resp.json().await.unwrap();
            assert_eq!(body["code"], json!("RATE_LIMITED"));
            seen_429 = true;
            break;
        }
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(seen_429, "连续失败应触发 429 限速");

    server.cleanup();
}
