//! WebDAV 协议端点的端到端行为（dav-server 实现 + axum 层 Basic 认证/路径加固）。
//!
//! 起一个真实监听端口的服务，用 reqwest 构造 PROPFIND/PUT/LOCK 等方法，
//! 覆盖认证、CRUD、Depth、Destination、锁语义与 Windows 路径安全门。

mod common;
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

impl Drop for TestServer {
    fn drop(&mut self) {
        // 收尾删临时目录（重试等句柄释放；删不掉就留给下一轮启动清）
        common::remove_dir_retried(&self.temp_dir);
    }
}

impl TestServer {
    async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// 按需覆盖配置启动（如缩短入站上传超时以快速触发取消路径）。
    async fn start_with(mutate_cfg: impl FnOnce(&mut AppConfig)) -> Self {
        common::clean_stale_temp_dirs();
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

        let mut cfg = AppConfig {
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
        mutate_cfg(&mut cfg);

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

    /// 用预设账号登录，返回 Access Token（JSON 接口端点走查询串鉴权）。
    async fn login_token(&self) -> String {
        let res = self
            .client
            .post(self.url("/reader3/login"))
            .json(&serde_json::json!({
                "username": TEST_USERNAME,
                "password": TEST_PASSWORD,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "登录应成功");
        let body: serde_json::Value = res.json().await.unwrap();
        body["data"]["accessToken"]
            .as_str()
            .expect("登录响应应含 accessToken")
            .to_string()
    }

    /// 用户 WebDAV 家目录（storage/webdav/<用户名>/）。
    fn home(&self) -> PathBuf {
        self.temp_dir.join("webdav").join("user_1")
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

/// 所有 401 都必须带 `WWW-Authenticate: Basic realm="reader"`：Windows
/// 资源管理器等客户端收不到质询头不会弹凭据框，表现为静默失败。
#[tokio::test]
async fn unauthorized_responses_carry_basic_challenge() {
    let app = TestServer::start().await;
    // 无凭据（缺 Authorization 头）
    for method in ["PROPFIND", "GET"] {
        let res = app
            .client
            .request(
                Method::from_bytes(method.as_bytes()).unwrap(),
                app.url("/reader3/webdav/"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            res.headers()
                .get("WWW-Authenticate")
                .and_then(|v| v.to_str().ok()),
            Some("Basic realm=\"reader\""),
            "{method} 无凭据 401 应带 Basic 质询头"
        );
    }
    // 密码错误
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
    assert_eq!(
        res.headers()
            .get("WWW-Authenticate")
            .and_then(|v| v.to_str().ok()),
        Some("Basic realm=\"reader\""),
        "错误凭据 401 应带 Basic 质询头"
    );
    // 非法 base64
    let res = app
        .client
        .request(
            Method::from_bytes(b"GET").unwrap(),
            app.url("/reader3/webdav/"),
        )
        .header("Authorization", "Basic !!!not-base64!!!")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get("WWW-Authenticate")
            .and_then(|v| v.to_str().ok()),
        Some("Basic realm=\"reader\""),
        "解码失败 401 应带 Basic 质询头"
    );
}

/// 下载响应必须带显式 Content-Type：裸流会被浏览器嗅探后同源渲染，
/// 未知类型一律 octet-stream 触发下载。
#[tokio::test]
async fn downloads_carry_explicit_content_type() {
    let app = TestServer::start().await;
    app.dav("PUT", "/reader3/webdav/note.txt")
        .body("hello")
        .send()
        .await
        .unwrap();
    app.dav("PUT", "/reader3/webdav/blob.dat")
        .body("blob")
        .send()
        .await
        .unwrap();

    // 协议端点（dav-server 的 mime_guess）
    let res = app
        .dav("GET", "/reader3/webdav/note.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/plain"),
        "协议 GET .txt 应带 text/plain"
    );
    let res = app
        .dav("GET", "/reader3/webdav/blob.dat")
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok()),
        Some("application/octet-stream"),
        "未知扩展名应为 octet-stream"
    );

    // JSON 接口端点（getWebdavFile 此前不带任何 Content-Type）
    let token = app.login_token().await;
    for (path, expected) in [
        ("/note.txt", "text/plain; charset=utf-8"),
        ("/blob.dat", "application/octet-stream"),
    ] {
        let res = app
            .client
            .get(app.url("/reader3/getWebdavFile"))
            .query(&[("path", path)])
            .header("Cookie", format!("reader_access={token}"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path} 应可下载");
        assert_eq!(
            res.headers()
                .get("Content-Type")
                .and_then(|v| v.to_str().ok()),
            Some(expected),
            "{path} 应显式带 {expected}"
        );
    }
}

/// .html/.xml/.svg 以可渲染类型同源直出，浏览器直接打开时内嵌脚本会在
/// 主站源下执行：协议端点与 getWebdavFile 都要补 Content-Security-Policy:
/// sandbox（保留渲染、禁脚本/表单）。纯文本下载不受这条规则影响。
#[tokio::test]
async fn active_content_downloads_get_sandbox_csp() {
    let app = TestServer::start().await;
    app.dav("PUT", "/reader3/webdav/note.html")
        .body("<html><script>alert(1)</script></html>")
        .send()
        .await
        .unwrap();

    // 协议端点（dav-server 的 mime_guess 给 text/html）
    let res = app
        .dav("GET", "/reader3/webdav/note.html")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let csp = res
        .headers()
        .get("Content-Security-Policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        csp.contains("sandbox"),
        "协议 GET .html 应带沙箱 CSP，实际: {csp:?}"
    );

    // JSON 接口端点（download_content_type 给 text/html; charset=utf-8）
    let token = app.login_token().await;
    let res = app
        .client
        .get(app.url("/reader3/getWebdavFile"))
        .query(&[("path", "/note.html")])
        .header("Cookie", format!("reader_access={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let csp = res
        .headers()
        .get("Content-Security-Policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        csp.contains("sandbox"),
        "getWebdavFile .html 应带沙箱 CSP，实际: {csp:?}"
    );

    // 纯文本不在活性类型清单里，不加这条 CSP
    app.dav("PUT", "/reader3/webdav/plain.txt")
        .body("hello")
        .send()
        .await
        .unwrap();
    let res = app
        .dav("GET", "/reader3/webdav/plain.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers().get("Content-Security-Policy").is_none(),
        "text/plain 不应带沙箱 CSP"
    );
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

    // 落盘位置不变：storage/webdav/user_<uid>/ 下的相对路径（JSON 接口依赖同一布局）
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

/// JSON 接口的 multipart 上传（uploadFileToWebdav）流式落盘：约 1.5MB 二进制
/// 逐字节一致（流式化改造前是整读进内存再 fs::write，这里用字节级比对守住
/// 回归）；同名覆盖上传后内容更新（覆盖 Windows 上 rename 的备份替换回退
/// 分支），且成功后不残留 `.upload-*.tmp` 临时文件与 `.reader-upload-backup-*`
/// 备份文件。
#[tokio::test]
async fn upload_file_to_webdav_streams_binary_and_overwrites() {
    let app = TestServer::start().await;
    let token = app.login_token().await;

    // 确定性伪随机字节：覆盖 0x00/0xFF 与 \r\n，确保不是碰巧能当文本处理的内容
    let payload: Vec<u8> = (0..1_500_000usize)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    let upload = |payload: &[u8]| {
        // reqwest 主 crate 没开 multipart feature：手工拼 multipart body
        let boundary = "----webdavuploadboundary";
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"path\"\r\n\r\n/backup\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"backup.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(payload);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        app.client
            .post(app.url("/reader3/uploadFileToWebdav"))
            .header("Cookie", format!("reader_access={token}"))
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
    };

    // 首次上传
    let res = upload(&payload).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "上传应成功");
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(true));
    assert_eq!(body["data"][0]["name"], "backup.bin");
    assert_eq!(body["data"][0]["size"], serde_json::json!(payload.len()));

    // 下载逐字节一致
    let res = app
        .client
        .get(app.url("/reader3/getWebdavFile"))
        .query(&[("path", "/backup/backup.bin")])
        .header("Cookie", format!("reader_access={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "下载应成功");
    let downloaded = res.bytes().await.unwrap();
    assert_eq!(
        downloaded.as_ref(),
        payload.as_slice(),
        "下载内容应与上传内容逐字节一致"
    );

    // 落盘内容一致，且不残留临时文件
    let disk = std::fs::read(app.home().join("backup").join("backup.bin")).unwrap();
    assert_eq!(disk, payload, "落盘内容应与上传内容逐字节一致");
    let leftovers: Vec<String> = std::fs::read_dir(app.home().join("backup"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".upload-") || name.starts_with(".reader-upload-backup-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "成功上传后不应残留临时/备份文件: {leftovers:?}"
    );

    // 同名覆盖：大小与内容都不同，落盘内容必须更新
    let payload2: Vec<u8> = (0..900_000usize)
        .map(|i| (i.wrapping_mul(17) % 249) as u8)
        .collect();
    let res = upload(&payload2).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "同名覆盖上传应成功");
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(true));
    assert_eq!(body["data"][0]["size"], serde_json::json!(payload2.len()));
    let disk = std::fs::read(app.home().join("backup").join("backup.bin")).unwrap();
    assert_eq!(disk, payload2, "覆盖上传后落盘内容应更新");
    let leftovers: Vec<String> = std::fs::read_dir(app.home().join("backup"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".upload-") || name.starts_with(".reader-upload-backup-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "覆盖上传后不应残留临时/备份文件: {leftovers:?}"
    );
}

/// 递归收集 webdav 家目录下所有 `.upload-*.tmp` 临时文件。
fn collect_upload_temps(root: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(collect_upload_temps(&path));
        } else if entry.file_name().to_string_lossy().contains(".upload-") {
            found.push(path.to_string_lossy().to_string());
        }
    }
    found
}

/// 中途断流的上传（客户端断连 / 上传超时取消 handler future）不得残留
/// `.upload-*.tmp` 临时文件：流式化改造后临时文件只在显式错误分支删除，
/// future 被丢弃时无人清理，孤儿文件会留在用户 WebDAV 主目录里并出现在
/// 文件列表中。守卫（Drop 清理）必须覆盖取消路径。
///
/// 用「发完 multipart 前缀后永久挂起的请求体流」复现取消：上传超时
/// （本测试缩短为 2s）触发 408，服务端丢弃 handler future。
#[tokio::test]
async fn aborted_upload_does_not_leak_temp_file() {
    let app = TestServer::start_with(|cfg| cfg.inbound_upload_timeout_secs = 2).await;
    let token = app.login_token().await;

    let boundary = "----webdavabortboundary";
    let mut prefix: Vec<u8> = Vec::new();
    prefix.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"path\"\r\n\r\n/backup\r\n")
            .as_bytes(),
    );
    prefix.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"leak.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    // 部分文件字节：让服务端已经建好临时文件、写了一部分，然后流挂起
    prefix.extend_from_slice(&[0xABu8; 8192]);

    let (mut tx, rx) = futures::channel::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(1);
    futures::SinkExt::send(&mut tx, Ok(bytes::Bytes::from(prefix)))
        .await
        .unwrap();
    // tx 不再发送也不 drop：请求体流永久挂起

    let res = app
        .client
        .post(app.url("/reader3/uploadFileToWebdav"))
        .header("Cookie", format!("reader_access={token}"))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(reqwest::Body::wrap_stream(rx))
        .send()
        .await;
    // 服务端 2s 超时后应答 408；客户端也可能先遇到连接被重置（提前应答 +
    // 请求体仍在流），两者都算取消路径已触发，断言重点在服务端清理
    match res {
        Ok(res) => assert_eq!(
            res.status(),
            StatusCode::REQUEST_TIMEOUT,
            "挂起的流式上传应被上传超时 408 掐断"
        ),
        Err(e) => eprintln!("客户端在收到 408 前连接被重置（可接受）: {e}"),
    }
    drop(tx);

    // 守卫在 future 析构时清理；留一点余量给句柄释放的延迟重试兜底
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let leftovers = collect_upload_temps(&app.home());
    assert!(
        leftovers.is_empty(),
        "取消的上传不应残留临时文件: {leftovers:?}"
    );
    // 半途而废的正文也不应出现成正式文件
    assert!(
        !app.home().join("backup").join("leak.bin").exists(),
        "中断的上传不得留下目标文件"
    );
}
