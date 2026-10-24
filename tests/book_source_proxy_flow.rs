//! bookSourceProxy 的校验顺序：scope/URL/出站守卫必须先于任何副作用。
//!
//! 回归点：被限定令牌拒绝的请求不能先把 Cookie 写进书源的会话存储。
//! 观测通道是 searchBook 的抓取路径（`fetch_source_url` 会带上存储的
//! source cookie 发起出站请求），上游用本地假服务记录收到的 Cookie 头。

mod common;
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use reader_rust::auth::jwt::{encode_token, Claims, PROXY_TOKEN_SCOPE};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const TEST_USERNAME: &str = "reader1";
const TEST_PASSWORD: &str = "password123";
const TEST_JWT_SECRET: &str = "integration-test-secret";

type CookieLog = Arc<Mutex<Vec<Option<String>>>>;

/// 假上游：把每个请求的 Cookie 头按到达顺序记录下来。
async fn start_upstream() -> (String, CookieLog) {
    let log: CookieLog = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log_for_route = log.clone();
    let app = axum::Router::new().route(
        "/*path",
        axum::routing::any(
            move |headers: axum::http::HeaderMap, _path: axum::extract::Path<String>| {
                let log = log_for_route.clone();
                async move {
                    log.lock().unwrap().push(
                        headers
                            .get(axum::http::header::COOKIE)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string),
                    );
                    axum::Json(serde_json::json!({}))
                }
            },
        ),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), log)
}

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
            std::env::temp_dir().join(format!("reader-rust-proxy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: TEST_JWT_SECRET.to_string(),
            admin_username: TEST_USERNAME.to_string(),
            admin_password: TEST_PASSWORD.to_string(),
            rate_limit_disabled: true,
            // 假上游监听 127.0.0.1：显式放行私网出站（默认拦截）
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
        let token = login(&client, &base_url).await;
        Self {
            base_url,
            temp_dir,
            client,
            token,
        }
    }

    async fn save_source(&self, name: &str, url: &str, search_url: &str) -> i64 {
        let resp = self
            .client
            .post(format!("{}/reader3/saveBookSource", self.base_url))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "bookSourceName": name,
                "bookSourceUrl": url,
                "searchUrl": search_url,
                "enabled": true,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "保存书源 {name} 应成功");
        let body: serde_json::Value = resp.json().await.unwrap();
        body["data"]["bookSourceId"]
            .as_i64()
            .expect("保存响应应含 bookSourceId")
    }

    /// 通过书源 B 触发一次 searchBook 抓取（fetch_source_url 会带上
    /// 存储的 source cookie 出站），用于观测 cookie 存储的当前内容。
    async fn trigger_search_fetch(&self, source_id: i64) {
        let _ = self
            .client
            .post(format!("{}/reader3/searchBook", self.base_url))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "bookSourceId": source_id,
                "key": "probe",
            }))
            .send()
            .await
            .unwrap();
    }

    /// 造一把绑定 bookSourceId 的限定代理令牌（与 mint_proxy_token 同构）。
    fn mint_proxy_token(&self, bound_source_id: i64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let claims = Claims {
            sub: TEST_USERNAME.to_string(),
            uid: 1,
            iat: now,
            exp: now + 600,
            ver: 0,
            scope: Some(PROXY_TOKEN_SCOPE.to_string()),
            bsid: Some(bound_source_id),
        };
        encode_token(&claims, TEST_JWT_SECRET.as_bytes()).unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

async fn login(client: &reqwest::Client, base_url: &str) -> String {
    let resp = client
        .post(format!("{base_url}/reader3/login"))
        .json(&serde_json::json!({
            "username": TEST_USERNAME,
            "password": TEST_PASSWORD,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    body["data"]["accessToken"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn rejected_proxy_request_writes_no_source_cookie() {
    let (upstream, cookie_log) = start_upstream().await;
    let server = TestServer::start().await;

    // 书源 B 指向本地假上游；书源 A 是另一个任意源
    let source_b = server
        .save_source(
            "源B",
            &upstream,
            &format!("{upstream}/search?key={{{{key}}}}&page={{{{page}}}}"),
        )
        .await;
    let source_a = server
        .save_source(
            "源A",
            "http://source-a.example",
            "http://source-a.example/s",
        )
        .await;

    // 绑定 A 的限定令牌去代理 B：应 400 拒绝
    let token_a = server.mint_proxy_token(source_a);
    let resp = server
        .client
        .get(format!("{}/reader3/bookSourceProxy", server.base_url))
        .bearer_auth(&token_a)
        .header("Cookie", "victim=1")
        .query(&[
            ("bookSourceId", source_b.to_string()),
            ("url", format!("{upstream}/probe")),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "令牌与书源不匹配应 400");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["errorMsg"], serde_json::json!("代理令牌与书源不匹配"));

    // 拒绝的请求不允许产生副作用：通过 B 抓一次搜索，上游不应看到 victim cookie
    server.trigger_search_fetch(source_b).await;
    {
        let log = cookie_log.lock().unwrap();
        assert!(
            log.iter()
                .all(|cookie| !cookie.as_deref().is_some_and(|c| c.contains("victim"))),
            "被拒绝的请求不应写入 source cookie，但上游收到了 {log:?}"
        );
    }

    // 对照组（证明观测通道本身有效）：主令牌的合法代理请求带 Cookie，
    // 会写入存储并被后续 searchBook 抓取带上
    let resp = server
        .client
        .get(format!("{}/reader3/bookSourceProxy", server.base_url))
        .bearer_auth(&server.token)
        .header("Cookie", "legit=1")
        .query(&[
            ("bookSourceId", source_b.to_string()),
            ("url", format!("{upstream}/probe")),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "合法代理请求应转发成功");

    server.trigger_search_fetch(source_b).await;
    {
        let log = cookie_log.lock().unwrap();
        assert!(
            log.iter()
                .any(|cookie| cookie.as_deref().is_some_and(|c| c.contains("legit=1"))),
            "合法请求写入的 cookie 应被 searchBook 抓取带上，实际 {log:?}"
        );
    }
}
