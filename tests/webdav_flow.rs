//! WebDAV 协议端点的端到端行为（dav-server 实现 + axum 层 Basic 认证/路径加固）。
//!
//! 起一个真实监听端口的服务，用 reqwest 构造 PROPFIND/PUT/LOCK 等方法，
//! 覆盖认证、CRUD、Depth、Destination、锁语义与 Windows 路径安全门。

use base64::Engine;
use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use reqwest::{Method, StatusCode};
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
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-webdav-{}", uuid::Uuid::new_v4()));
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

    fn auth_header(&self) -> String {
        let raw = format!("{TEST_USERNAME}:{TEST_PASSWORD}");
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(raw)
        )
    }

    /// 构造一个带 Basic 认证的 WebDAV 方法请求。
    fn dav(&self, method: &str, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(
                Method::from_bytes(method.as_bytes()).unwrap(),
                self.url(path),
            )
            .header("Authorization", self.auth_header())
    }

    /// 用户 WebDAV 家目录（storage/webdav/<用户名>/）。
    fn home(&self) -> PathBuf {
        self.temp_dir.join("webdav").join(TEST_USERNAME)
    }
}

#[tokio::test]
async fn unauthenticated_requests_get_401() {
    let app = TestServer::start().await;
    for method in ["PROPFIND", "PUT", "GET", "DELETE", "MKCOL"] {
        let res = app
            .client
            .request(
                Method::from_bytes(method.as_bytes()).unwrap(),
                app.url("/reader3/webdav/"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "{method} 未认证应 401"
        );
    }
    // 错误的 Basic 凭据同样 401（注意不能复用 dav()，它会带上合法凭据）
    let bad = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("reader1:wrong")
    );
    let res = app
        .client
        .request(
            Method::from_bytes(b"PROPFIND").unwrap(),
            app.url("/reader3/webdav/"),
        )
        .header("Authorization", bad)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn propfind_root_bare_and_trailing_slash() {
    let app = TestServer::start().await;
    // 裸根路径与带尾斜杠都要可用：WebDAV 客户端两种都会发
    for path in ["/reader3/webdav", "/reader3/webdav/"] {
        let res = app
            .dav("PROPFIND", path)
            .header("Depth", "1")
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::from_u16(207).unwrap(),
            "{path} 应返回 207"
        );
        let body = res.text().await.unwrap();
        assert!(body.contains("multistatus"), "{path} 应为 multistatus XML");
    }
    // OPTIONS 被全站 CORS 层直接应答（替换前后行为一致），只验证可达性
    let res = app.dav("OPTIONS", "/reader3/webdav/").send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn mkcol_put_get_head_roundtrip() {
    let app = TestServer::start().await;

    let res = app
        .dav("MKCOL", "/reader3/webdav/dir")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    // RFC 4918 §9.3.1：对已存在的集合返回 405。
    // dav-server 0.11 在 Windows 上的 errno→FsError 映射用 libc 的 CRT 常量比对
    // Win32 错误码，EEXIST 对不上（CRT 17 vs Win32 80），已存在目录会落成 500；
    // Linux（部署目标）无此问题。
    let res = app
        .dav("MKCOL", "/reader3/webdav/dir")
        .send()
        .await
        .unwrap();
    if cfg!(windows) {
        assert!(
            res.status() == StatusCode::METHOD_NOT_ALLOWED
                || res.status() == StatusCode::INTERNAL_SERVER_ERROR,
            "重复 MKCOL 应为 405（Windows 上容忍 dav-server 的 500），实际 {}",
            res.status()
        );
    } else {
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    let res = app
        .dav("PUT", "/reader3/webdav/dir/a.txt")
        .body("hello dav")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let res = app
        .dav("GET", "/reader3/webdav/dir/a.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.text().await.unwrap(), "hello dav");

    let res = app
        .dav("HEAD", "/reader3/webdav/dir/a.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 覆盖写
    let res = app
        .dav("PUT", "/reader3/webdav/dir/a.txt")
        .body("v2")
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success());
    let res = app
        .dav("GET", "/reader3/webdav/dir/a.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.text().await.unwrap(), "v2");

    // 落盘位置不变：storage/webdav/<用户名>/ 下的相对路径（JSON 接口依赖同一布局）
    assert_eq!(
        std::fs::read_to_string(app.home().join("dir").join("a.txt")).unwrap(),
        "v2"
    );
}

#[tokio::test]
async fn propfind_respects_depth() {
    let app = TestServer::start().await;
    app.dav("MKCOL", "/reader3/webdav/dir")
        .send()
        .await
        .unwrap();
    app.dav("PUT", "/reader3/webdav/dir/a.txt")
        .body("x")
        .send()
        .await
        .unwrap();

    let res = app
        .dav("PROPFIND", "/reader3/webdav/dir")
        .header("Depth", "0")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::from_u16(207).unwrap());
    let body = res.text().await.unwrap();
    assert!(!body.contains("a.txt"), "Depth:0 不应包含子条目: {body}");

    let res = app
        .dav("PROPFIND", "/reader3/webdav/dir")
        .header("Depth", "1")
        .send()
        .await
        .unwrap();
    let body = res.text().await.unwrap();
    assert!(body.contains("a.txt"), "Depth:1 应包含子条目: {body}");
}

#[tokio::test]
async fn move_and_copy_with_destination_header() {
    let app = TestServer::start().await;
    app.dav("PUT", "/reader3/webdav/a.txt")
        .body("data")
        .send()
        .await
        .unwrap();

    // Destination 完整 URL 形式（RFC 4918 规定客户端发绝对 URI）
    let res = app
        .dav("MOVE", "/reader3/webdav/a.txt")
        .header("Destination", app.url("/reader3/webdav/b.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let res = app
        .dav("GET", "/reader3/webdav/a.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = app
        .dav("GET", "/reader3/webdav/b.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.text().await.unwrap(), "data");

    // Destination 绝对路径形式
    let res = app
        .dav("COPY", "/reader3/webdav/b.txt")
        .header("Destination", "/reader3/webdav/c.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let res = app
        .dav("GET", "/reader3/webdav/c.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.text().await.unwrap(), "data");
}

#[tokio::test]
async fn delete_file_and_missing() {
    let app = TestServer::start().await;
    app.dav("PUT", "/reader3/webdav/del.txt")
        .body("x")
        .send()
        .await
        .unwrap();
    let res = app
        .dav("DELETE", "/reader3/webdav/del.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let res = app
        .dav("DELETE", "/reader3/webdav/del.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_dangerous_paths() {
    let app = TestServer::start().await;

    // percent 编码的路径穿越：axum Path 解码后过安全门
    let res = app
        .dav("PUT", "/reader3/webdav/..%2f..%2fevil.txt")
        .body("x")
        .send()
        .await
        .unwrap();
    assert!(
        res.status().is_client_error(),
        "穿越路径应被 4xx 拒绝，实际 {}",
        res.status()
    );
    assert!(
        !app.temp_dir.join("evil.txt").exists(),
        "文件不得落在家目录之外"
    );

    // Windows 保留设备名与结尾点（dav-server 的 LocalFs 不认识，靠我们的安全门）
    let res = app
        .dav("PUT", "/reader3/webdav/NUL")
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = app
        .dav("PUT", "/reader3/webdav/a.txt.")
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn lock_blocks_write_until_unlock() {
    let app = TestServer::start().await;
    app.dav("PUT", "/reader3/webdav/lockme.txt")
        .body("v1")
        .send()
        .await
        .unwrap();

    let lock_body = r#"<?xml version="1.0" encoding="utf-8"?><D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockinfo>"#;
    let res = app
        .dav("LOCK", "/reader3/webdav/lockme.txt")
        .header("Content-Type", "application/xml")
        .body(lock_body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let token = res
        .headers()
        .get("Lock-Token")
        .expect("LOCK 响应应携带 Lock-Token")
        .to_str()
        .unwrap()
        .to_string();

    // 真实锁（MemLs）：无锁令牌的写入被拒绝
    let res = app
        .dav("PUT", "/reader3/webdav/lockme.txt")
        .body("v2")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::LOCKED);

    let res = app
        .dav("UNLOCK", "/reader3/webdav/lockme.txt")
        .header("Lock-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // 解锁后可写
    let res = app
        .dav("PUT", "/reader3/webdav/lockme.txt")
        .body("v2")
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success());
}
