//! 鉴权、路由回落与缓存清理的端到端行为。
//!
//! 起一个真实监听端口的服务（而不是直接调 handler），这样中间件分层、
//! 状态码映射与静态回落都能被真实地覆盖到。

use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use std::path::PathBuf;

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
}

impl TestServer {
    async fn start() -> Self {
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        // 静态资源根目录：只放一个 index.html，用于验证「不回落」行为
        let web_root = temp_dir.join("dist");
        std::fs::create_dir_all(&web_root).unwrap();
        std::fs::write(
            web_root.join("index.html"),
            "<!doctype html><title>app</title>",
        )
        .unwrap();
        std::fs::write(web_root.join("sw.js"), "// service worker").unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: web_root.to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "integration-test-secret".to_string(),
            // 集成测试从同一地址注册多个账号，豁免 IP 限速
            rate_limit_disabled: true,
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

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// 注册（或登录）一个账号，返回 JWT。
    async fn register(&self, username: &str) -> String {
        let response = self
            .client
            .post(self.url("/reader3/login"))
            .json(&serde_json::json!({
                "username": username,
                "password": "password123",
                "isLogin": false,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "注册应成功: {username}");
        let body: serde_json::Value = response.json().await.unwrap();
        body["data"]["accessToken"]
            .as_str()
            .expect("注册响应应含 accessToken")
            .to_string()
    }

    async fn get(&self, path: &str, token: Option<&str>) -> reqwest::Response {
        let mut request = self.client.get(self.url(path));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap()
    }

    async fn post(
        &self,
        path: &str,
        token: Option<&str>,
        body: serde_json::Value,
    ) -> reqwest::Response {
        let mut request = self.client.post(self.url(path)).json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

#[tokio::test]
async fn protected_routes_require_a_token_and_answer_401() {
    let server = TestServer::start().await;

    let response = server.get("/reader3/getBookshelf", None).await;
    assert_eq!(response.status(), 401, "缺少令牌应返回 401 而不是 400");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(false));
    // 保留 NEED_LOGIN 供前端拦截器识别
    assert_eq!(body["errorMsg"], serde_json::json!("NEED_LOGIN"));

    // 无效令牌同样 401
    let response = server.get("/reader3/getBookshelf", Some("not-a-jwt")).await;
    assert_eq!(response.status(), 401);

    // 有效令牌放行
    let token = server.register("reader1").await;
    let response = server.get("/reader3/getBookshelf", Some(&token)).await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn token_is_accepted_from_the_query_parameter_for_event_source() {
    let server = TestServer::start().await;
    let token = server.register("reader1").await;

    // EventSource 无法设置请求头，只能把令牌放进查询串
    let response = server
        .client
        .get(server.url(&format!("/reader3/getBookshelf?accessToken={token}")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn admin_routes_forbid_non_admins_with_403() {
    let server = TestServer::start().await;
    // 首个注册者自动成为管理员
    let admin = server.register("admin1").await;
    let member = server.register("member1").await;

    let response = server.get("/reader3/getUserList", Some(&member)).await;
    assert_eq!(response.status(), 403, "非管理员应返回 403");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["errorMsg"], serde_json::json!("FORBIDDEN"));

    let response = server.get("/reader3/getUserList", Some(&admin)).await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let users = body["data"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    // 用户列表不得回吐任何人的凭据
    for user in users {
        assert!(user.get("accessToken").is_none(), "列表不应含 accessToken");
    }
}

#[tokio::test]
async fn changing_password_revokes_old_tokens_but_keeps_the_current_device() {
    let server = TestServer::start().await;
    let old_token = server.register("reader1").await;

    let response = server
        .post(
            "/reader3/changePassword",
            Some(&old_token),
            serde_json::json!({ "oldPassword": "password123", "newPassword": "newpassword123" }),
        )
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let new_token = body["data"]["accessToken"]
        .as_str()
        .expect("改密码应换发新令牌")
        .to_string();

    assert_eq!(
        server
            .get("/reader3/getBookshelf", Some(&old_token))
            .await
            .status(),
        401,
        "旧令牌应因版本号自增而失效"
    );
    assert_eq!(
        server
            .get("/reader3/getBookshelf", Some(&new_token))
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn unknown_api_paths_answer_json_404_instead_of_falling_back_to_static() {
    let server = TestServer::start().await;

    let response = server.get("/reader3/definitelyNotAnEndpoint", None).await;
    assert_eq!(response.status(), 404);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(false));

    // 非 API 的未知路径同样是 404，且不回落 index.html
    let response = server.get("/some/deep/link", None).await;
    assert_eq!(response.status(), 404);
    let text = response.text().await.unwrap();
    assert!(!text.contains("<title>app</title>"), "不应回落 index.html");
}

#[tokio::test]
async fn root_and_real_static_files_are_still_served() {
    let server = TestServer::start().await;

    let response = server.get("/", None).await;
    assert_eq!(response.status(), 200);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("<title>app</title>"));

    // dist 根目录下的 PWA 资源按文件名直接可取
    let response = server.get("/sw.js", None).await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn purge_cache_respects_ownership_and_admin_only_scopes() {
    let server = TestServer::start().await;
    // 首个注册者自动成为管理员，因此先注册管理员再注册普通用户
    let admin = server.register("admin1").await;
    let member = server.register("member1").await;

    // 普通用户可清理自己的命名空间
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&member),
            serde_json::json!({ "scope": "user" }),
        )
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let purged = &body["data"]["purged"];
    for layer in [
        "content",
        "cover",
        "chapterList",
        "searchResults",
        "invalidSources",
    ] {
        assert!(purged[layer].is_number(), "缺少 {layer} 计数");
    }

    // 按类型清理自己的某一层
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&member),
            serde_json::json!({ "scope": "kind", "kind": "content" }),
        )
        .await;
    assert_eq!(response.status(), 200);

    // 单本书的清理需要 bookUrl
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&member),
            serde_json::json!({ "scope": "book" }),
        )
        .await;
    assert_eq!(response.status(), 400, "缺少 bookUrl 应被拒绝");
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&member),
            serde_json::json!({ "scope": "book", "bookUrl": "https://example.com/book/1" }),
        )
        .await;
    assert_eq!(response.status(), 200);

    // 普通用户不能清理他人，也不能全局清理
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&member),
            serde_json::json!({ "scope": "user", "username": "admin1" }),
        )
        .await;
    assert_eq!(response.status(), 403);
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&member),
            serde_json::json!({ "scope": "all" }),
        )
        .await;
    assert_eq!(response.status(), 403);

    // 管理员可以指定他人、也可以全局清理
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&admin),
            serde_json::json!({ "scope": "user", "username": "member1" }),
        )
        .await;
    assert_eq!(response.status(), 200);
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&admin),
            serde_json::json!({ "scope": "all" }),
        )
        .await;
    assert_eq!(response.status(), 200);

    // 统计接口：普通用户只能看自己，全局统计仅管理员可用
    assert_eq!(
        server
            .get("/reader3/cacheStats", Some(&member))
            .await
            .status(),
        200
    );
    assert_eq!(
        server
            .get("/reader3/cacheStats?all=true", Some(&member))
            .await
            .status(),
        403
    );
    assert_eq!(
        server
            .get("/reader3/cacheStats?all=true", Some(&admin))
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn logout_always_succeeds_even_with_an_invalid_token() {
    let server = TestServer::start().await;

    // 令牌已过期/无效时登出也应返回成功：服务端无可撤销状态，
    // 这里只是给客户端一个「可以清本地令牌」的确认
    let response = server
        .post("/reader3/logout", Some("expired"), serde_json::json!({}))
        .await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn get_user_info_reports_no_session_when_anonymous() {
    let server = TestServer::start().await;

    let response = server.get("/reader3/getUserInfo", None).await;
    assert_eq!(response.status(), 200, "未登录也应可调用");
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["data"]["userInfo"].is_null());
    assert_eq!(body["data"]["adminAuthorized"], serde_json::json!(false));

    let token = server.register("reader1").await;
    let response = server.get("/reader3/getUserInfo", Some(&token)).await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["data"]["userInfo"]["username"],
        serde_json::json!("reader1")
    );
}
