//! 鉴权、路由回落与缓存清理的端到端行为。
//!
//! 起一个真实监听端口的服务（而不是直接调 handler），这样中间件分层、
//! 状态码映射与静态回落都能被真实地覆盖到。

mod common;
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use reader_rust::auth::jwt::decode_token;
use std::path::PathBuf;

/// 测试账号：由 bootstrap 用预设的 ADMIN_USERNAME/ADMIN_PASSWORD 创建。
const TEST_USERNAME: &str = "reader1";
const TEST_PASSWORD: &str = "password123";

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
}

impl TestServer {
    async fn start() -> Self {
        common::clean_stale_temp_dirs();
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
            // 预设凭据，bootstrap 直接建号，测试拿到确定密码
            admin_username: TEST_USERNAME.to_string(),
            admin_password: TEST_PASSWORD.to_string(),
            // 同一地址会重复登录，豁免 IP 限速
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

    /// 登录并返回 (JSON data, Set-Cookie 列表)。
    ///
    /// 令牌已迁移到 HttpOnly Cookie：refreshToken 只出现在 Set-Cookie 里，
    /// JSON 保留 accessToken（API 客户端 Bearer 用）。reqwest 默认不开
    /// cookie store，Set-Cookie 由测试手动接管。
    async fn login_with_cookies(&self) -> (serde_json::Value, Vec<String>) {
        let response = self
            .client
            .post(self.url("/reader3/login"))
            .json(&serde_json::json!({
                "username": TEST_USERNAME,
                "password": TEST_PASSWORD,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "登录应成功");
        let cookies = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_string))
            .collect();
        let body: serde_json::Value = response.json().await.unwrap();
        (body["data"].clone(), cookies)
    }

    /// 用预设账号登录，只返回 Access Token（JSON 仍保留它供 Bearer 客户端）。
    async fn login(&self) -> String {
        self.login_with_cookies().await.0["accessToken"]
            .as_str()
            .expect("登录响应应含 accessToken")
            .to_string()
    }

    /// 登录并把两枚会话 Cookie 拼成请求 Cookie 头（浏览器语义的手工等价物）。
    async fn login_cookie_header(&self) -> String {
        let (_, cookies) = self.login_with_cookies().await;
        cookie_header(&cookies)
    }

    async fn get(&self, path: &str, token: Option<&str>) -> reqwest::Response {
        self.get_with_cookie(path, token, None).await
    }

    async fn get_with_cookie(
        &self,
        path: &str,
        token: Option<&str>,
        cookie: Option<&str>,
    ) -> reqwest::Response {
        let mut request = self.client.get(self.url(path));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
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

/// 从 Set-Cookie 列表取指定名称的完整条目（含属性）。
fn set_cookie_of<'a>(cookies: &'a [String], name: &str) -> Option<&'a String> {
    cookies
        .iter()
        .find(|cookie| cookie.starts_with(&format!("{name}=")))
}

/// 从 Set-Cookie 条目取裸值（等号后、首个分号前）。
fn set_cookie_value(cookie: &str) -> &str {
    cookie
        .split(';')
        .next()
        .unwrap_or("")
        .split_once('=')
        .map(|(_, value)| value)
        .unwrap_or("")
}

/// 把响应的 Set-Cookie 列表拼成请求 Cookie 头。
fn cookie_header(cookies: &[String]) -> String {
    cookies
        .iter()
        .map(|cookie| cookie.split(';').next().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

#[tokio::test]
async fn protected_routes_require_a_token_and_answer_401() {
    let server = TestServer::start().await;

    let response = server.get("/reader3/getBookshelf", None).await;
    assert_eq!(response.status(), 401, "缺少令牌应返回 401 而不是 400");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(false));
    // 前端凭 401 识别登录失效；errorMsg 会被原样 toast，必须是用户可读的文案
    assert_eq!(body["errorMsg"], serde_json::json!("需要登录"));

    // 无效令牌同样 401
    let response = server.get("/reader3/getBookshelf", Some("not-a-jwt")).await;
    assert_eq!(response.status(), 401);

    // 有效令牌放行
    let token = server.login().await;
    let response = server.get("/reader3/getBookshelf", Some(&token)).await;
    assert_eq!(response.status(), 200);
}

/// 查询串令牌只保留给书源登录代理页的限定令牌：其他路径（含 SSE）一律
/// 忽略，浏览器端点改由 HttpOnly Cookie 鉴权。
#[tokio::test]
async fn query_token_is_only_accepted_on_proxy_paths() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // SSE 等端点曾依赖查询串令牌（EventSource 无法设置请求头），现在走 Cookie
    let response = server
        .client
        .get(server.url(&format!("/reader3/getBookshelf?accessToken={token}")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401, "非代理路径应忽略查询串令牌");

    // 代理路径仍接受查询串令牌：主 JWT 无 scope，鉴权通过后由 handler 报缺参数
    let response = server
        .client
        .get(server.url(&format!("/reader3/bookSourceProxy?accessToken={token}")))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        400,
        "代理路径的查询串令牌应被接受（此处因缺 bookSourceId 报 400，而不是 401）"
    );
}

#[tokio::test]
async fn login_rejects_wrong_password_and_unknown_users() {
    let server = TestServer::start().await;

    let response = server
        .post(
            "/reader3/login",
            None,
            serde_json::json!({ "username": TEST_USERNAME, "password": "wrong-password" }),
        )
        .await;
    assert_eq!(response.status(), 400);

    // 不存在的用户名同样报「用户名或密码错误」，不提供注册入口
    let response = server
        .post(
            "/reader3/login",
            None,
            serde_json::json!({ "username": "nobody", "password": "password123" }),
        )
        .await;
    assert_eq!(response.status(), 400);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["errorMsg"]
        .as_str()
        .unwrap()
        .contains("用户名或密码错误"));
}

#[tokio::test]
async fn multi_user_admin_endpoints_are_gone() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // 多用户时代的管理端点已删除，落在 JSON 404 上而不是 403
    let response = server.get("/reader3/getUserList", Some(&token)).await;
    assert_eq!(response.status(), 404);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(false));
}

#[tokio::test]
async fn changing_password_revokes_old_tokens_but_keeps_the_current_device() {
    let server = TestServer::start().await;
    let old_token = server.login().await;

    let response = server
        .post(
            "/reader3/changePassword",
            Some(&old_token),
            serde_json::json!({ "oldPassword": TEST_PASSWORD, "newPassword": "newpassword123" }),
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
async fn login_issues_short_lived_access_and_rotating_refresh_token() {
    let server = TestServer::start().await;
    let (data, cookies) = server.login_with_cookies().await;

    let access = data["accessToken"].as_str().expect("应含 accessToken");
    let claims = decode_token(access, b"integration-test-secret").unwrap();
    assert_eq!(claims.sub, TEST_USERNAME);
    assert_eq!(
        claims.exp - claims.iat,
        6 * 3600,
        "Access Token 默认 6 小时"
    );
    assert_eq!(data["accessTokenExpiresIn"], serde_json::json!(6 * 3600));

    // refreshToken 不再交给 JS：JSON 里没有，只在 HttpOnly Cookie 里
    assert!(
        data.get("refreshToken").is_none(),
        "JSON 不应再返回 refreshToken"
    );
    let refresh_cookie =
        set_cookie_of(&cookies, "reader_refresh").expect("登录应下发 reader_refresh Cookie");
    let refresh = set_cookie_value(refresh_cookie);
    assert!(refresh.starts_with("rt_"));
    assert_ne!(refresh, access, "Refresh Token 不能是 Access Token 本身");
}

/// Cookie 属性契约：HttpOnly、SameSite=Strict、Path=/reader3、Max-Age 取
/// 各自 TTL；host-only（不设 Domain）。
#[tokio::test]
async fn session_cookies_carry_the_contracted_attributes() {
    let server = TestServer::start().await;
    let (_, cookies) = server.login_with_cookies().await;

    let access = set_cookie_of(&cookies, "reader_access").expect("应下发 reader_access");
    for attr in [
        "HttpOnly",
        "SameSite=Strict",
        "Path=/reader3",
        "Max-Age=21600",
    ] {
        assert!(
            access.contains(attr),
            "reader_access 缺属性 {attr}: {access}"
        );
    }
    assert!(
        !access.contains("Domain="),
        "host-only Cookie 不能设 Domain"
    );

    let refresh = set_cookie_of(&cookies, "reader_refresh").expect("应下发 reader_refresh");
    for attr in [
        "HttpOnly",
        "SameSite=Strict",
        "Path=/reader3",
        "Max-Age=604800",
    ] {
        assert!(
            refresh.contains(attr),
            "reader_refresh 缺属性 {attr}: {refresh}"
        );
    }
    assert!(!refresh.contains("Domain="));
}

/// 受保护接口只带 Cookie（无 Bearer）也能通过；不带任何凭据仍 401。
#[tokio::test]
async fn protected_routes_accept_the_session_cookie_alone() {
    let server = TestServer::start().await;
    let cookie = server.login_cookie_header().await;

    let response = server
        .get_with_cookie("/reader3/getBookshelf", None, Some(&cookie))
        .await;
    assert_eq!(response.status(), 200, "仅凭 Cookie 应通过鉴权");

    // 对照：不带任何凭据 401
    let response = server.get("/reader3/getBookshelf", None).await;
    assert_eq!(response.status(), 401);
}

/// 图片接口（<img> 无法设置请求头）改由 Cookie 鉴权：无凭据 401，
/// 带 Cookie 到达业务层（此处按非法 id 报 404，无网络依赖）。
#[tokio::test]
async fn image_endpoint_accepts_the_session_cookie() {
    let server = TestServer::start().await;
    let cookie = server.login_cookie_header().await;

    let response = server
        .get_with_cookie("/reader3/image/zz", None, None)
        .await;
    assert_eq!(response.status(), 401, "无凭据应在鉴权层被拦下");

    let response = server
        .get_with_cookie("/reader3/image/zz", None, Some(&cookie))
        .await;
    assert_eq!(
        response.status(),
        404,
        "带 Cookie 应通过鉴权并到达业务校验（非法 id 报 404）"
    );
}

/// 刷新轮换与令牌不可互换。Refresh Token 只认 `reader_refresh` Cookie，
/// body 提交的 `refreshToken` 一律忽略（旧客户端/CLI 回退已删除）。
#[tokio::test]
async fn refresh_rotates_tokens_via_cookie_and_types_stay_interchangeable() {
    let server = TestServer::start().await;
    let (data, cookies) = server.login_with_cookies().await;
    let access = data["accessToken"].as_str().unwrap().to_string();
    let refresh = set_cookie_value(set_cookie_of(&cookies, "reader_refresh").unwrap()).to_string();

    // body 里的 refreshToken 不再受理：没有 Cookie 即 401
    let response = server
        .post(
            "/reader3/refreshToken",
            None,
            serde_json::json!({ "refreshToken": &refresh }),
        )
        .await;
    assert_eq!(response.status(), 401, "body 提交的 refreshToken 应被忽略");

    // Access Token 不能用来换新令牌（放到 Cookie 位置同样查不到）
    let response = server
        .client
        .post(server.url("/reader3/refreshToken"))
        .header("Cookie", format!("reader_refresh={access}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    // Refresh Token 不是 JWT，不能当访问凭据
    assert_eq!(
        server
            .get("/reader3/getBookshelf", Some(&refresh))
            .await
            .status(),
        401
    );

    // Cookie 里的 Refresh Token 正常换新
    let response = server
        .client
        .post(server.url("/reader3/refreshToken"))
        .header("Cookie", format!("reader_refresh={refresh}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let new_access = body["data"]["accessToken"].as_str().unwrap().to_string();
    assert!(
        body["data"].get("refreshToken").is_none(),
        "刷新响应的 JSON 不应再返回 refreshToken"
    );
    assert_eq!(
        server
            .get("/reader3/getBookshelf", Some(&new_access))
            .await
            .status(),
        200
    );

    // 已消费令牌在 30 秒宽限窗口内重放仍可换取（断网重试语义）
    assert_eq!(
        server
            .client
            .post(server.url("/reader3/refreshToken"))
            .header("Cookie", format!("reader_refresh={refresh}"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // 伪造令牌一律 401
    assert_eq!(
        server
            .client
            .post(server.url("/reader3/refreshToken"))
            .header("Cookie", "reader_refresh=rt_forged")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

/// 浏览器主路径：refresh 走 Cookie，成功后轮换出新的两枚 Cookie。
#[tokio::test]
async fn refresh_via_cookie_rotates_the_cookie_pair() {
    let server = TestServer::start().await;
    let (_, cookies) = server.login_with_cookies().await;
    let refresh_cookie = set_cookie_of(&cookies, "reader_refresh").unwrap().clone();
    let cookie = cookie_header(&cookies);

    // 空 body + Cookie（前端契约：body 正常为空对象）
    let response = server
        .client
        .post(server.url("/reader3/refreshToken"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let new_cookies: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect();
    let new_refresh =
        set_cookie_of(&new_cookies, "reader_refresh").expect("刷新应换发 reader_refresh");
    assert_ne!(
        set_cookie_value(new_refresh),
        set_cookie_value(&refresh_cookie),
        "换取后必须下发新的 Refresh Token"
    );
    // Access 是确定性 JWT（同秒签发值相同），只验证换发动作本身存在
    assert!(
        set_cookie_of(&new_cookies, "reader_access").is_some(),
        "刷新应同时换发 reader_access"
    );
    // 新 Cookie 属性与登录时一致
    for attr in [
        "HttpOnly",
        "SameSite=Strict",
        "Path=/reader3",
        "Max-Age=604800",
    ] {
        assert!(
            new_refresh.contains(attr),
            "新 reader_refresh 缺属性 {attr}: {new_refresh}"
        );
    }

    // 新 Cookie 立即可用
    let response = server
        .get_with_cookie(
            "/reader3/getBookshelf",
            None,
            Some(&cookie_header(&new_cookies)),
        )
        .await;
    assert_eq!(response.status(), 200);
}

/// 刷新失败（伪造/无凭据）统一 401 并清除两枚 Cookie。
#[tokio::test]
async fn failed_refresh_clears_both_cookies() {
    let server = TestServer::start().await;

    // 伪造 Cookie
    let response = server
        .client
        .post(server.url("/reader3/refreshToken"))
        .header(
            "Cookie",
            "reader_refresh=rt_forged; reader_access=forged.jwt",
        )
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let cookies: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect();
    assert_eq!(cookies.len(), 2, "失败时应清除两枚 Cookie: {cookies:?}");
    for name in ["reader_access", "reader_refresh"] {
        let cleared = set_cookie_of(&cookies, name).expect("应包含清除 {name} 的 Set-Cookie");
        assert!(
            cleared.contains("Max-Age=0"),
            "{name} 应以 Max-Age=0 清除: {cleared}"
        );
    }

    // 完全没有凭据：同样 401 + 清除
    let response = server
        .client
        .post(server.url("/reader3/refreshToken"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert!(
        response.headers().get_all("set-cookie").iter().count() == 2,
        "无凭据的刷新也应清除 Cookie"
    );
}

#[tokio::test]
async fn password_change_revokes_refresh_tokens_and_issues_a_new_pair() {
    let server = TestServer::start().await;
    let (data, cookies) = server.login_with_cookies().await;
    let access = data["accessToken"].as_str().unwrap().to_string();
    let old_refresh =
        set_cookie_value(set_cookie_of(&cookies, "reader_refresh").unwrap()).to_string();

    let response = server
        .post(
            "/reader3/changePassword",
            Some(&access),
            serde_json::json!({ "oldPassword": TEST_PASSWORD, "newPassword": "newpassword123" }),
        )
        .await;
    assert_eq!(response.status(), 200);
    // 新 refresh 只在 Set-Cookie 里（json() 会消耗 response，先取头）
    let new_cookies: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect();
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body["data"].get("refreshToken").is_none(),
        "改密响应的 JSON 不应再返回 refreshToken"
    );
    let new_refresh = set_cookie_value(
        set_cookie_of(&new_cookies, "reader_refresh").expect("改密应换发 reader_refresh"),
    )
    .to_string();
    assert_ne!(new_refresh, old_refresh);

    assert_eq!(
        server
            .client
            .post(server.url("/reader3/refreshToken"))
            .header("Cookie", format!("reader_refresh={old_refresh}"))
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "改密码后旧 Refresh Token 必须失效"
    );
    assert_eq!(
        server
            .client
            .post(server.url("/reader3/refreshToken"))
            .header("Cookie", format!("reader_refresh={new_refresh}"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn logout_revokes_the_refresh_family_clears_cookies_and_stays_idempotent() {
    let server = TestServer::start().await;
    let (_, cookies) = server.login_with_cookies().await;
    let refresh = set_cookie_value(set_cookie_of(&cookies, "reader_refresh").unwrap()).to_string();
    let cookie = cookie_header(&cookies);

    // 浏览器路径：凭 Cookie 登出
    let response = server
        .client
        .post(server.url("/reader3/logout"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cleared: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect();
    for name in ["reader_access", "reader_refresh"] {
        let entry = set_cookie_of(&cleared, name).unwrap_or_else(|| panic!("登出应清除 {name}"));
        assert!(
            entry.contains("Max-Age=0"),
            "{name} 应以 Max-Age=0 清除: {entry}"
        );
    }

    // 该会话家族已吊销：旧 refresh 再换不到新令牌
    assert_eq!(
        server
            .client
            .post(server.url("/reader3/refreshToken"))
            .header("Cookie", format!("reader_refresh={refresh}"))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "登出后该会话家族应被吊销"
    );

    // 无 body 的登出仍然成功：客户端本地已无令牌时也走这条路（幂等）
    let response = server
        .client
        .post(server.url("/reader3/logout"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers().get_all("set-cookie").iter().count() == 2,
        "幂等登出同样要清 Cookie"
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
async fn font_shards_get_immutable_cache_while_plain_files_revalidate() {
    let server = TestServer::start().await;

    // 字体目录不在默认骨架里，现场补一个带内容 hash 文件名的分片
    let fonts_dir = server.temp_dir.join("dist").join("fonts").join("misans");
    std::fs::create_dir_all(&fonts_dir).unwrap();
    std::fs::write(
        fonts_dir.join("MiSansVF.0123456789abcdef.0.woff2"),
        b"fake-font",
    )
    .unwrap();

    let response = server
        .get("/fonts/misans/MiSansVF.0123456789abcdef.0.woff2", None)
        .await;
    assert_eq!(response.status(), 200);
    let cache_control = response
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        cache_control.contains("immutable"),
        "字体分片应带 immutable 缓存头，实际: {cache_control}"
    );

    // 分片索引样式表（约 96KB）：内容长期稳定，与分片同档 immutable，
    // 走 /fonts.css 显式路由而不是 fallback
    std::fs::write(
        server.temp_dir.join("dist").join("fonts.css"),
        "@font-face{font-family:MiSans}",
    )
    .unwrap();
    let response = server.get("/fonts.css", None).await;
    assert_eq!(response.status(), 200);
    let cache_control = response
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        cache_control.contains("immutable"),
        "fonts.css 应带 immutable 缓存头，实际: {cache_control}"
    );

    // 无 hash 的根级文件保持每次重验证，不会被 fonts 规则误伤
    let response = server.get("/sw.js", None).await;
    let cache_control = response
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        cache_control.contains("no-cache"),
        "sw.js 应保持 no-cache，实际: {cache_control}"
    );
}

#[tokio::test]
async fn purge_cache_scopes_all_work_for_the_single_user() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // 按命名空间清理
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&token),
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

    // 按类型清理某一层（正文历史不提供清理，这里用封面）
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&token),
            serde_json::json!({ "scope": "kind", "kind": "cover" }),
        )
        .await;
    assert_eq!(response.status(), 200);

    // 单本书的清理需要 bookId
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&token),
            serde_json::json!({ "scope": "book" }),
        )
        .await;
    assert_eq!(response.status(), 400, "缺少 bookId 应被拒绝");
    // bookId 拼缓存文件路径：未登记/非法格式的 id 一律 404，到不了文件系统
    for book_id in ["book_999", "../../data", "book_1/../book_2"] {
        let response = server
            .post(
                "/reader3/purgeCache",
                Some(&token),
                serde_json::json!({ "scope": "book", "bookId": book_id }),
            )
            .await;
        assert_eq!(
            response.status(),
            404,
            "未登记或非法的 bookId {book_id:?} 应 404"
        );
    }

    // 单用户即所有者：全局清理与全局统计都直接可用
    let response = server
        .post(
            "/reader3/purgeCache",
            Some(&token),
            serde_json::json!({ "scope": "all" }),
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        server
            .get("/reader3/cacheStats?all=true", Some(&token))
            .await
            .status(),
        200
    );
}

/// saveBooks（整架替换/恢复）按现存分组掩码剥掉悬空位：
/// 备份里旧版本的非法组 id、已被删的组，不能在书架上留成幽灵分组。
#[tokio::test]
async fn save_books_masks_group_bits_to_existing_groups() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // 只建组 1
    let response = server
        .post(
            "/reader3/saveBookGroup",
            Some(&token),
            serde_json::json!({ "groupId": 0, "groupName": "组一", "orderNo": 0 }),
        )
        .await;
    assert_eq!(response.status(), 200);

    // 书籍位掩码 0b111：组 1 合法，位 2/4 是悬空位
    let response = server
        .post(
            "/reader3/saveBooks",
            Some(&token),
            serde_json::json!({
                "books": [{
                    "bookId": "book_1",
                    "bookUrl": "https://example.com/group-mask",
                    "name": "掩码测试",
                    "origin": "https://example.com",
                    "group": 0b111,
                }],
                "restore": true,
            }),
        )
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["data"][0]["group"],
        serde_json::json!(1),
        "悬空位应被剥掉，只剩组 1 的位"
    );
}

#[tokio::test]
async fn logout_always_succeeds_even_with_an_invalid_token() {
    let server = TestServer::start().await;

    // 令牌已过期/无效时登出也应返回成功：Refresh Token 查不到就静默跳过，
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

    let token = server.login().await;
    let response = server.get("/reader3/getUserInfo", Some(&token)).await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["data"]["userInfo"]["username"],
        serde_json::json!(TEST_USERNAME)
    );
    // 响应不含任何凭据
    assert!(body["data"]["userInfo"].get("accessToken").is_none());
    assert!(body["data"]["userInfo"].get("password").is_none());
}

#[tokio::test]
async fn multipart_upload_rejects_excessive_field_count() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // 单字段有字节限量，但字段数没有上限时，一个请求挂上千个字段照样打爆配额。
    // 统一上限 20 个字段（api/handlers/multipart.rs::MAX_MULTIPART_FIELDS）
    let boundary = "----fieldlimitboundary";
    let mut body = String::new();
    for index in 0..25 {
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"f{index}\"\r\n\r\nx\r\n"
        ));
    }
    body.push_str(&format!("--{boundary}--\r\n"));

    let response = server
        .client
        .post(server.url("/reader3/uploadTxtBook"))
        .bearer_auth(&token)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400, "超字段数上限应 400");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(false));

    // 正常单字段上传不受影响
    let mut body = String::new();
    body.push_str(&format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"正常.txt\"\r\nContent-Type: text/plain\r\n\r\n第一章 开始\n正文。\r\n"
    ));
    body.push_str(&format!("--{boundary}--\r\n"));
    let response = server
        .client
        .post(server.url("/reader3/uploadTxtBook"))
        .bearer_auth(&token)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "正常上传不应被误伤");
}

#[tokio::test]
async fn public_login_body_is_capped_at_1mb() {
    let server = TestServer::start().await;

    // 未登录就能触达的端点：2MB 的垃圾 JSON 应被公开组 1MB 上限挡成 413，
    // 而不是按全局兜底先缓冲 100MB 再进业务逻辑
    let big_password = "x".repeat(2 * 1024 * 1024);
    let response = server
        .client
        .post(server.url("/reader3/login"))
        .json(&serde_json::json!({
            "username": TEST_USERNAME,
            "password": big_password,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413, "超过公开组 1MB 上限应返回 413");

    // 正常小 body 仍能拿到业务响应
    let response = server
        .client
        .post(server.url("/reader3/login"))
        .json(&serde_json::json!({
            "username": TEST_USERNAME,
            "password": TEST_PASSWORD,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "小 body 不应被上限误伤");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(true));
}

/// previewChapterImport 与 importChapterText 共用 read_chapter_import_request
/// （TXT 上限 50MB），预览此前留在 protected 的 8MB 组：8-50MB 的 TXT
/// 在预览一步就 413。移入大请求体组后，9MB 请求应到达业务逻辑（400）。
#[tokio::test]
async fn preview_chapter_import_accepts_bodies_over_8mb() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // 约 9MB 的 TXT：超过旧的 8MB 组上限，低于 TXT 的 50MB 字段上限
    let boundary = "----previewbigbodyboundary";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.txt\"\r\nContent-Type: text/plain\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&vec![b'x'; 9 * 1024 * 1024]);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let response = server
        .client
        .post(server.url("/reader3/previewChapterImport"))
        .bearer_auth(&token)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_ne!(
        response.status(),
        413,
        "9MB 的预览请求不应被请求体上限挡成 413"
    );
    // 缺 bookId 的业务 400：证明请求体已完整读入、走到业务校验
    assert_eq!(response.status(), 400);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["errorMsg"].as_str().unwrap().contains("bookId"));
}
