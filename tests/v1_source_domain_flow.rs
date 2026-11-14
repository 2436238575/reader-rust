//! 书源域 v1 收尾端点：文件解析/远端拉取/导出、登录/代理票据、换源落地。

mod common;

use axum::http::StatusCode;
use serde_json::{json, Value};
use std::path::PathBuf;

use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;

const JWT_SECRET: &str = "v1-source-domain-secret";
const PROXY_COOKIE: &str = "sid=secret-cookie-value; Path=/";

struct TestServer {
    base_url: String,
    temp_dir: PathBuf,
    client: reqwest::Client,
    token: String,
}

impl TestServer {
    async fn start(fake_upstream: &str) -> Self {
        common::clean_stale_temp_dirs();
        let temp_dir = std::env::temp_dir().join(format!(
            "reader-rust-v1-source-domain-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let cfg = AppConfig {
            allow_private_network: true,
            request_timeout_secs: 1,
            ..common::base_config(&temp_dir, JWT_SECRET)
        };
        let state = build_state(cfg).await.unwrap();
        let app = build_router(state);
        let (base_url, _server) = common::spawn_app(app).await;
        let client = reqwest::Client::new();
        let token = common::login(&client, &base_url).await;
        let _ = fake_upstream;
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

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = self
            .client
            .request(method, self.url(path))
            .bearer_auth(&self.token)
            .header("x-requested-with", "XMLHttpRequest");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, value)
    }

    async fn upload_sources(&self, filename: &str, content: &[u8]) -> (StatusCode, Value) {
        let boundary = "----source-domain-test";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/json\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(content);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let response = self
            .client
            .post(self.url("/v1/sources:parse"))
            .bearer_auth(&self.token)
            .header("x-requested-with", "XMLHttpRequest")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, value)
    }
}

/// 假上游：`/login` 下发一个 Cookie，`/sources.json` 返回一份书源。
async fn fake_upstream() -> (String, tokio::task::JoinHandle<()>) {
    use axum::routing::get;
    use axum::Router;
    let app = Router::new()
        .route(
            "/login",
            get(|| async {
                (
                    StatusCode::OK,
                    [("set-cookie", PROXY_COOKIE)],
                    "<html>login</html>",
                )
            }),
        )
        .route(
            "/sources.json",
            get(|| async {
                axum::Json(json!([{
                    "bookSourceName": "远端源",
                    "bookSourceUrl": "https://remote.example"
                }]))
            }),
        )
        .route(
            "/echo-cookie",
            get(|headers: axum::http::HeaderMap| async move {
                headers
                    .get("cookie")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

async fn source_id_by_name(server: &TestServer, name: &str) -> String {
    let (status, page) = server
        .req(reqwest::Method::GET, "/v1/sources?limit=200", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == json!(name))
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("找不到书源 {name}: {page}"))
        .to_string()
}

#[tokio::test]
async fn source_login_reports_ok_and_cookie_count_without_cookie_values() {
    let (upstream, _handle) = fake_upstream().await;
    let server = TestServer::start(&upstream).await;
    let (status, _) = server
        .req(
            reqwest::Method::POST,
            "/v1/sources:batchUpsert",
            Some(json!({
                "sources": [
                    {
                        "bookSourceName": "登录成功源",
                        "bookSourceUrl": upstream.clone(),
                        "loginUrl": format!("{upstream}/login")
                    },
                    {
                        "bookSourceName": "登录失败源",
                        "bookSourceUrl": format!("{upstream}/fail"),
                        "loginUrl": format!("{upstream}/login"),
                        "loginCheckJs": "false"
                    }
                ]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let ok_id = source_id_by_name(&server, "登录成功源").await;
    let (status, body) = server
        .req(
            reqwest::Method::POST,
            &format!("/v1/sources/{ok_id}/login"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["cookieCount"], json!(1));
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("1 个 Cookie"),
        "{body}"
    );
    assert!(
        body["previewUrl"]
            .as_str()
            .is_some_and(|url| url.ends_with("/login")),
        "{body}"
    );
    // 不泄露 Cookie 值
    assert!(!body.to_string().contains("secret-cookie-value"), "{body}");

    let fail_id = source_id_by_name(&server, "登录失败源").await;
    let (status, problem) = server
        .req(
            reqwest::Method::POST,
            &format!("/v1/sources/{fail_id}/login"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["code"], json!("SOURCE_LOGIN_FAILED"));
    assert!(!problem.to_string().contains("secret-cookie-value"));

    server.cleanup();
}

#[tokio::test]
async fn source_parse_and_export_and_strict_fetch() {
    let (upstream, _handle) = fake_upstream().await;
    let server = TestServer::start(&upstream).await;

    let (status, parsed) = server
        .upload_sources(
            "sources.json",
            r#"[{"bookSourceName":"解析源","bookSourceUrl":"https://parse.example"}]"#.as_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{parsed}");
    assert_eq!(parsed[0]["bookSourceName"], json!("解析源"));

    // 只解析不落库：列表仍为空
    let (_, page) = server.req(reqwest::Method::GET, "/v1/sources", None).await;
    assert_eq!(page["total"], json!(0));

    let (status, problem) = server.upload_sources("bad.xml", b"[]").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    let (status, problem) = server.upload_sources("bad.json", b"not json").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");

    // 导出：空列表也是合法数组，带 attachment
    let response = server
        .client
        .get(server.url("/v1/sources:export"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get("content-disposition")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("attachment;")));
    let exported: Value = response.json().await.unwrap();
    assert_eq!(exported, json!([]));

    // 严格公网：即使 ALLOW_PRIVATE_NETWORK=true，也拒绝环回地址
    let (status, problem) = server
        .req(
            reqwest::Method::POST,
            "/v1/sources:fetch",
            Some(json!({ "url": format!("{upstream}/sources.json") })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert_eq!(problem["code"], json!("UPSTREAM_BLOCKED"));

    let (status, problem) = server
        .req(
            reqwest::Method::POST,
            "/v1/sources:fetch",
            Some(json!({ "url": "file:///etc/passwd" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");

    server.cleanup();
}

#[tokio::test]
async fn source_proxy_token_is_ten_minutes_and_bound_to_source() {
    let (upstream, _handle) = fake_upstream().await;
    let server = TestServer::start(&upstream).await;
    let (status, _) = server
        .req(
            reqwest::Method::POST,
            "/v1/sources:batchUpsert",
            Some(json!({
                "sources": [{
                    "bookSourceName": "票据源",
                    "bookSourceUrl": upstream
                }]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = source_id_by_name(&server, "票据源").await;
    let numeric = id.trim_start_matches("src_").parse::<i64>().unwrap();

    let (status, body) = server
        .req(
            reqwest::Method::POST,
            &format!("/v1/sources/{id}/proxy-token"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["expiresIn"], json!(600));
    let token = body["token"].as_str().unwrap();
    let claims = reader_rust::auth::jwt::decode_token(token, JWT_SECRET.as_bytes()).unwrap();
    assert_eq!(claims.scope.as_deref(), Some("bookSourceProxy"));
    assert_eq!(claims.bsid, Some(numeric));
    assert!(claims.exp - claims.iat <= 600);

    server.cleanup();
}

#[tokio::test]
async fn change_book_source_rebinds_and_keeps_progress() {
    let (upstream, _handle) = fake_upstream().await;
    let server = TestServer::start(&upstream).await;
    let (status, _) = server
        .req(
            reqwest::Method::POST,
            "/v1/sources:batchUpsert",
            Some(json!({
                "sources": [{
                    "bookSourceName": "换源源",
                    "bookSourceUrl": upstream
                }]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let source_id = source_id_by_name(&server, "换源源").await;
    let numeric: i64 = source_id.trim_start_matches("src_").parse().unwrap();

    let save_book = |url: &str, name: &str| {
        json!({
            "name": name,
            "author": "作者",
            "bookUrl": url,
            "tocUrl": format!("{url}/toc"),
            "bookSourceId": numeric
        })
    };
    let (_, current) = server
        .req(
            reqwest::Method::POST,
            "/reader3/saveBook",
            Some(save_book("https://cur.example/book", "当前书")),
        )
        .await;
    let current_id = current["data"]["bookId"].as_str().unwrap().to_string();
    let (_, target) = server
        .req(
            reqwest::Method::POST,
            "/reader3/saveBook",
            Some(save_book("https://target.example/book", "目标书")),
        )
        .await;
    let target_id = target["data"]["bookId"].as_str().unwrap().to_string();
    let (_, third) = server
        .req(
            reqwest::Method::POST,
            "/reader3/saveBook",
            Some(save_book("https://third.example/book", "第三书")),
        )
        .await;
    let third_id = third["data"]["bookId"].as_str().unwrap().to_string();

    // 目标书必须已登记但不在书架
    let (status, _) = server
        .req(
            reqwest::Method::DELETE,
            &format!("/v1/me/shelf/{target_id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // 当前书设置进度
    let (status, _) = server
        .req(
            reqwest::Method::PATCH,
            &format!("/v1/me/shelf/{current_id}"),
            Some(json!({ "progress": { "chapterIndex": 3, "offset": 0.5 } })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, saved) = server
        .req(
            reqwest::Method::PUT,
            &format!("/v1/books/{current_id}/source"),
            Some(json!({ "targetBookId": target_id })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["bookId"], json!(current_id));
    assert_eq!(saved["bookUrl"], json!("https://target.example/book"));
    assert_eq!(saved["durChapterIndex"], json!(3));

    // 同书 422；目标仍在架 409；目标不存在 404
    let (status, problem) = server
        .req(
            reqwest::Method::PUT,
            &format!("/v1/books/{current_id}/source"),
            Some(json!({ "targetBookId": current_id })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    let (status, problem) = server
        .req(
            reqwest::Method::PUT,
            &format!("/v1/books/{current_id}/source"),
            Some(json!({ "targetBookId": third_id })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    let (status, problem) = server
        .req(
            reqwest::Method::PUT,
            &format!("/v1/books/{current_id}/source"),
            Some(json!({ "targetBookId": "book_999999" })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");

    server.cleanup();
}

#[tokio::test]
async fn proxy_never_forwards_reader_session_cookies() {
    let (upstream, _handle) = fake_upstream().await;
    let server = TestServer::start(&upstream).await;
    let (status, _) = server
        .req(
            reqwest::Method::POST,
            "/v1/sources:batchUpsert",
            Some(json!({
                "sources": [{
                    "bookSourceName": "代理源",
                    "bookSourceUrl": upstream
                }]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = source_id_by_name(&server, "代理源").await;
    let numeric: i64 = id.trim_start_matches("src_").parse().unwrap();
    let (status, token_body) = server
        .req(
            reqwest::Method::POST,
            &format!("/v1/sources/{id}/proxy-token"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{token_body}");
    let token = token_body["token"].as_str().unwrap();

    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("url", &format!("{upstream}/echo-cookie"))
        .append_pair("bookSourceId", &numeric.to_string())
        .append_pair("accessToken", token)
        .finish();
    let cookie = format!(
        "reader_access={}; reader_refresh=refresh-should-not-leak; sid=upstream-session",
        server.token
    );
    let response = server
        .client
        .get(server.url(&format!("/reader3/bookSourceProxy?{query}")))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("sid=upstream-session"), "{body}");
    assert!(
        !body.contains("reader_access") && !body.contains(&server.token),
        "主 JWT 不能转发给书源: {body}"
    );
    assert!(!body.contains("reader_refresh"), "{body}");

    server.cleanup();
}
