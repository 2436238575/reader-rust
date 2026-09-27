//! userdata（跨浏览器同步的最近阅读 / 阅读统计载体）接口的端到端行为。

use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use std::path::PathBuf;

const TEST_USERNAME: &str = "reader1";
const TEST_PASSWORD: &str = "password123";

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
}

impl TestServer {
    async fn start() -> Self {
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-userdata-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let cfg = AppConfig {
            server_host: "127.0.0.1".to_string(),
            server_port: 0,
            database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
            storage_dir: temp_dir.to_string_lossy().to_string(),
            web_root: temp_dir.join("dist").to_string_lossy().to_string(),
            assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
            jwt_secret: "integration-test-secret".to_string(),
            admin_username: TEST_USERNAME.to_string(),
            admin_password: TEST_PASSWORD.to_string(),
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

    async fn login(&self) -> String {
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
        let body: serde_json::Value = response.json().await.unwrap();
        body["data"]["accessToken"]
            .as_str()
            .expect("登录响应应含 accessToken")
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
async fn userdata_requires_a_token() {
    let server = TestServer::start().await;

    let response = server.get("/reader3/getUserdata?name=recent-books", None).await;
    assert_eq!(response.status(), 401);

    let response = server
        .post(
            "/reader3/saveUserdata",
            None,
            serde_json::json!({ "name": "recent-books", "value": [] }),
        )
        .await;
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn userdata_roundtrip_save_overwrite_and_missing() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // 未保存过的文档返回 null
    let response = server
        .get("/reader3/getUserdata?name=reading-stats", Some(&token))
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["isSuccess"].as_bool().unwrap());
    assert!(body["data"].is_null());

    // 保存对象文档并读回
    let response = server
        .post(
            "/reader3/saveUserdata",
            Some(&token),
            serde_json::json!({
                "name": "reading-stats",
                "value": { "totalSeconds": 3600, "openedBooks": ["a", "b"] },
            }),
        )
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["isSuccess"].as_bool().unwrap(), "保存应成功: {body}");

    let response = server
        .get("/reader3/getUserdata?name=reading-stats", Some(&token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["totalSeconds"], 3600);
    assert_eq!(body["data"]["openedBooks"], serde_json::json!(["a", "b"]));

    // 整文档覆盖写
    let response = server
        .post(
            "/reader3/saveUserdata",
            Some(&token),
            serde_json::json!({
                "name": "reading-stats",
                "value": { "totalSeconds": 7200, "openedBooks": ["a", "b", "c"] },
            }),
        )
        .await;
    assert_eq!(response.status(), 200);

    let response = server
        .get("/reader3/getUserdata?name=reading-stats", Some(&token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["totalSeconds"], 7200);
    assert_eq!(body["data"]["openedBooks"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn userdata_rejects_unsafe_or_invalid_names() {
    let server = TestServer::start().await;
    let token = server.login().await;

    for name in ["", "..", "a/b", "a\\b", ".hidden", "包含中文"] {
        let response = server
            .post(
                "/reader3/saveUserdata",
                Some(&token),
                serde_json::json!({ "name": name, "value": { "x": 1 } }),
            )
            .await;
        assert_eq!(response.status(), 200, "非法名不应 5xx: {name}");
        let body: serde_json::Value = response.json().await.unwrap();
        assert!(
            !body["isSuccess"].as_bool().unwrap(),
            "非法名应被拒绝: {name}"
        );

        let query = urlencoding::encode(name);
        let response = server
            .get(&format!("/reader3/getUserdata?name={query}"), Some(&token))
            .await;
        let body: serde_json::Value = response.json().await.unwrap();
        assert!(
            !body["isSuccess"].as_bool().unwrap(),
            "非法名读取也应被拒绝: {name}"
        );
    }
}

#[tokio::test]
async fn userdata_accepts_array_documents() {
    let server = TestServer::start().await;
    let token = server.login().await;

    let response = server
        .post(
            "/reader3/saveUserdata",
            Some(&token),
            serde_json::json!({
                "name": "recent-books",
                "value": [
                    { "bookUrl": "https://example.com/1", "origin": "https://example.com", "name": "书一", "recentReadAt": 100 },
                    { "bookUrl": "https://example.com/2", "origin": "https://example.com", "name": "书二", "recentReadAt": 200 },
                ],
            }),
        )
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["isSuccess"].as_bool().unwrap(), "数组文档应可保存: {body}");

    let response = server
        .get("/reader3/getUserdata?name=recent-books", Some(&token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    let list = body["data"].as_array().expect("应返回数组文档");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["name"], "书一");
}
