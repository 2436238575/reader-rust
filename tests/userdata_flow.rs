//! userdata（跨浏览器同步的最近阅读 / 阅读统计载体）接口的端到端行为。

mod common;
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
        common::clean_stale_temp_dirs();
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
        common::remove_dir_retried(&self.temp_dir);
    }
}

#[tokio::test]
async fn userdata_requires_a_token() {
    let server = TestServer::start().await;

    let response = server
        .get("/reader3/getUserdata?name=recent-books", None)
        .await;
    assert_eq!(response.status(), 401);

    let response = server
        .post(
            "/reader3/saveUserdata",
            None,
            serde_json::json!({ "name": "recent-books", "value": [] }),
        )
        .await;
    assert_eq!(response.status(), 401);

    let response = server
        .get("/reader3/getUserdataBatch?names=recent-books", None)
        .await;
    assert_eq!(response.status(), 401);

    let response = server
        .post(
            "/reader3/saveUserdataBatch",
            None,
            serde_json::json!({ "docs": [{ "name": "recent-books", "value": [] }] }),
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
        // 错误通道单轨化后：参数错误一律 HTTP 400 + isSuccess=false
        assert_eq!(response.status(), 400, "非法名应 400: {name}");
        let body: serde_json::Value = response.json().await.unwrap();
        assert!(
            !body["isSuccess"].as_bool().unwrap(),
            "非法名应被拒绝: {name}"
        );

        let query = urlencoding::encode(name);
        let response = server
            .get(&format!("/reader3/getUserdata?name={query}"), Some(&token))
            .await;
        assert_eq!(response.status(), 400, "非法名读取应 400: {name}");
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
    assert!(
        body["isSuccess"].as_bool().unwrap(),
        "数组文档应可保存: {body}"
    );

    let response = server
        .get("/reader3/getUserdata?name=recent-books", Some(&token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    let list = body["data"].as_array().expect("应返回数组文档");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["name"], "书一");
}

#[tokio::test]
async fn user_config_roundtrip_and_legacy_file_migration() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // 保存 → 读取往返（进 json_documents 表）
    let response = server
        .post(
            "/reader3/saveUserConfig",
            Some(&token),
            serde_json::json!({ "readMode": "night", "fontSize": 20 }),
        )
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["isSuccess"].as_bool().unwrap(), "保存应成功: {body}");

    let response = server.get("/reader3/getUserConfig", Some(&token)).await;
    let body: serde_json::Value = response.json().await.unwrap();
    let data = &body["data"];
    assert_eq!(data["readMode"], "night");
    assert_eq!(data["fontSize"], 20);
    assert!(data["@updateTime"].as_i64().unwrap() > 0, "应带更新时间戳");

    // 老文件迁移路径用全新服务验证（新 storage：表里空、文件有）
    let legacy_server = TestServer::start().await;
    let legacy_token = legacy_server.login().await;
    let legacy_dir = legacy_server.temp_dir.join("data").join("user_1");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    let legacy_file = legacy_dir.join("userConfig.json");
    std::fs::write(&legacy_file, r#"{"readMode":"day","legacy":true}"#).unwrap();
    let response = legacy_server
        .get("/reader3/getUserConfig", Some(&legacy_token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["legacy"], true, "应吃到老文件内容");
    assert!(!legacy_file.exists(), "迁移后老文件应被删除");
    // 再读一次走表
    let response = legacy_server
        .get("/reader3/getUserConfig", Some(&legacy_token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["readMode"], "day", "迁移后应读表");
}

#[tokio::test]
async fn oversized_user_documents_are_rejected() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // 5MB 的 userdata 文档：到达 handler 后按 4MB 上限拒绝（用户可读文案）
    let big = "x".repeat(5 * 1024 * 1024);
    let response = server
        .post(
            "/reader3/saveUserdata",
            Some(&token),
            serde_json::json!({ "name": "reading-stats", "value": { "blob": big } }),
        )
        .await;
    assert_eq!(response.status(), 400, "超过 4MB 的 userdata 应 400");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(false));
    let msg = body["errorMsg"].as_str().unwrap();
    assert!(msg.contains("4MB"), "错误文案应说明上限：{msg}");

    // 5MB 的 userConfig 同样拒绝
    let response = server
        .post(
            "/reader3/saveUserConfig",
            Some(&token),
            serde_json::json!({ "blob": big }),
        )
        .await;
    assert_eq!(response.status(), 400, "超过 4MB 的 userConfig 应 400");

    // 1MB 的文档正常保存并可读回
    let ok_blob = "y".repeat(1024 * 1024);
    let response = server
        .post(
            "/reader3/saveUserdata",
            Some(&token),
            serde_json::json!({ "name": "reading-stats", "value": { "blob": ok_blob } }),
        )
        .await;
    assert_eq!(response.status(), 200, "1MB 文档应正常保存");
    let response = server
        .get("/reader3/getUserdata?name=reading-stats", Some(&token))
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["data"]["blob"].as_str().map(str::len),
        Some(1024 * 1024)
    );
}

#[tokio::test]
async fn userdata_batch_save_then_individual_and_batch_get() {
    let server = TestServer::start().await;
    let token = server.login().await;

    let response = server
        .post(
            "/reader3/saveUserdataBatch",
            Some(&token),
            serde_json::json!({ "docs": [
                { "name": "reading-stats", "value": { "totalSeconds": 3600 } },
                { "name": "recent-books", "value": [
                    { "bookUrl": "https://example.com/1", "name": "书一", "recentReadAt": 100 },
                ] },
                { "name": "read-marks", "value": { "https://example.com/1": [1, 2, 3] } },
            ]}),
        )
        .await;
    assert_eq!(response.status(), 200, "批量保存应成功");
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body["isSuccess"].as_bool().unwrap(),
        "批量保存应成功: {body}"
    );
    assert_eq!(body["data"]["saved"], 3, "应报告写入 3 个文档");

    // 逐个单文档 get 可见
    let response = server
        .get("/reader3/getUserdata?name=reading-stats", Some(&token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["totalSeconds"], 3600);
    let response = server
        .get("/reader3/getUserdata?name=read-marks", Some(&token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["data"]["https://example.com/1"],
        serde_json::json!([1, 2, 3])
    );

    // 批量 get 一并读回
    let response = server
        .get(
            "/reader3/getUserdataBatch?names=recent-books,read-marks,reading-stats",
            Some(&token),
        )
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let docs = body["data"]["docs"]
        .as_object()
        .expect("data.docs 应是映射");
    assert_eq!(docs.len(), 3);
    assert_eq!(docs["reading-stats"]["totalSeconds"], 3600);
    assert_eq!(
        docs["read-marks"]["https://example.com/1"],
        serde_json::json!([1, 2, 3])
    );
    assert_eq!(docs["recent-books"][0]["name"], "书一");
}

#[tokio::test]
async fn userdata_batch_invalid_name_rejects_whole_batch() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // 混入非法文档名：整批 400，合法的那个也不能被写入
    let response = server
        .post(
            "/reader3/saveUserdataBatch",
            Some(&token),
            serde_json::json!({ "docs": [
                { "name": "reading-stats", "value": { "totalSeconds": 1 } },
                { "name": "../evil", "value": { "x": 1 } },
            ]}),
        )
        .await;
    assert_eq!(response.status(), 400, "混入非法名的批量保存应 400");
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(!body["isSuccess"].as_bool().unwrap());

    let response = server
        .get("/reader3/getUserdata?name=reading-stats", Some(&token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["data"].is_null(), "被拒的批量不应留下部分写入");

    // 批量 get 里的非法名同样 400（逐个校验，任一非法即拒）
    let query = urlencoding::encode("../evil");
    let response = server
        .get(
            &format!("/reader3/getUserdataBatch?names=reading-stats,{query}"),
            Some(&token),
        )
        .await;
    assert_eq!(response.status(), 400, "批量 get 混入非法名应 400");

    // 空 names / 空 docs 都是 400
    let response = server
        .get("/reader3/getUserdataBatch?names=", Some(&token))
        .await;
    assert_eq!(response.status(), 400, "空 names 应 400");
    let response = server.get("/reader3/getUserdataBatch", Some(&token)).await;
    assert_eq!(response.status(), 400, "缺 names 参数应 400");
    let response = server
        .post(
            "/reader3/saveUserdataBatch",
            Some(&token),
            serde_json::json!({ "docs": [] }),
        )
        .await;
    assert_eq!(response.status(), 400, "空 docs 应 400");

    // 批量保存同样受单文档 4MB 上限约束，且超限时整批拒绝
    let big = "x".repeat(5 * 1024 * 1024);
    let response = server
        .post(
            "/reader3/saveUserdataBatch",
            Some(&token),
            serde_json::json!({ "docs": [
                { "name": "small-doc", "value": { "ok": true } },
                { "name": "big-doc", "value": { "blob": big } },
            ]}),
        )
        .await;
    assert_eq!(response.status(), 400, "混入超限文档的批量应 400");
    let response = server
        .get("/reader3/getUserdata?name=small-doc", Some(&token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["data"].is_null(), "超限批量不应留下部分写入");
}

#[tokio::test]
async fn userdata_batch_get_missing_docs_returns_null() {
    let server = TestServer::start().await;
    let token = server.login().await;

    server
        .post(
            "/reader3/saveUserdataBatch",
            Some(&token),
            serde_json::json!({ "docs": [{ "name": "exists", "value": 42 }] }),
        )
        .await;

    let response = server
        .get(
            "/reader3/getUserdataBatch?names=exists,missing-a,missing-b",
            Some(&token),
        )
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    let docs = body["data"]["docs"]
        .as_object()
        .expect("data.docs 应是映射");
    assert_eq!(docs.len(), 3, "缺失文档也应出现在映射里");
    assert_eq!(docs["exists"], 42);
    assert!(docs["missing-a"].is_null(), "缺失文档应为 null");
    assert!(docs["missing-b"].is_null(), "缺失文档应为 null");
}

#[tokio::test]
async fn userdata_batch_rejects_more_than_twenty_docs() {
    let server = TestServer::start().await;
    let token = server.login().await;

    let twenty_one_names = (0..21)
        .map(|i| format!("doc-{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let response = server
        .get(
            &format!("/reader3/getUserdataBatch?names={twenty_one_names}"),
            Some(&token),
        )
        .await;
    assert_eq!(response.status(), 400, "21 个名字的批量 get 应 400");

    let twenty_one_docs = (0..21)
        .map(|i| serde_json::json!({ "name": format!("doc-{i}"), "value": i }))
        .collect::<Vec<_>>();
    let response = server
        .post(
            "/reader3/saveUserdataBatch",
            Some(&token),
            serde_json::json!({ "docs": twenty_one_docs }),
        )
        .await;
    assert_eq!(response.status(), 400, "21 个文档的批量保存应 400");
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(!body["isSuccess"].as_bool().unwrap());
    // 拒绝后一个都没写
    let response = server
        .get("/reader3/getUserdata?name=doc-0", Some(&token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["data"].is_null());

    // 20 个恰好放行
    let twenty_docs = (0..20)
        .map(|i| serde_json::json!({ "name": format!("doc-{i}"), "value": i }))
        .collect::<Vec<_>>();
    let response = server
        .post(
            "/reader3/saveUserdataBatch",
            Some(&token),
            serde_json::json!({ "docs": twenty_docs }),
        )
        .await;
    assert_eq!(response.status(), 200, "20 个文档应放行");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["saved"], 20);
}

#[tokio::test]
async fn userdata_batch_duplicate_name_last_write_wins() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // 同名文档按最后一次为准：只写一份，值为后者
    let response = server
        .post(
            "/reader3/saveUserdataBatch",
            Some(&token),
            serde_json::json!({ "docs": [
                { "name": "reading-stats", "value": { "totalSeconds": 1 } },
                { "name": "other-doc", "value": "kept" },
                { "name": "reading-stats", "value": { "totalSeconds": 99 } },
            ]}),
        )
        .await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["saved"], 2, "去重后应只写 2 个文档");

    let response = server
        .get("/reader3/getUserdata?name=reading-stats", Some(&token))
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["totalSeconds"], 99, "同名文档应取最后一次的值");

    // 批量 get 的 names 含重复时，映射里同名只出现一次
    let response = server
        .get(
            "/reader3/getUserdataBatch?names=reading-stats,reading-stats,other-doc",
            Some(&token),
        )
        .await;
    let body: serde_json::Value = response.json().await.unwrap();
    let docs = body["data"]["docs"].as_object().unwrap();
    assert_eq!(docs.len(), 2);
    assert_eq!(docs["reading-stats"]["totalSeconds"], 99);
    assert_eq!(docs["other-doc"], "kept");
}
