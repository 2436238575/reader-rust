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
            .post(self.url("/v1/auth/login"))
            .header("x-requested-with", "XMLHttpRequest")
            .json(&serde_json::json!({
                "username": TEST_USERNAME,
                "password": TEST_PASSWORD,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "登录应成功");
        let body: serde_json::Value = res.json().await.unwrap();
        body["accessToken"]
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
    assert_eq!(
        body["isSuccess"],
        serde_json::json!(true),
        "上传应直接成功: {body}"
    );
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
        .filter(|name| name.contains(".upload-") || name.contains(".reader-upload-backup-"))
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
    assert_eq!(
        body["isSuccess"],
        serde_json::json!(true),
        "上传应成功: {body}"
    );
    assert_eq!(body["data"][0]["size"], serde_json::json!(payload2.len()));
    let disk = std::fs::read(app.home().join("backup").join("backup.bin")).unwrap();
    assert_eq!(disk, payload2, "覆盖上传后落盘内容应更新");
    let leftovers: Vec<String> = std::fs::read_dir(app.home().join("backup"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".upload-") || name.contains(".reader-upload-backup-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "覆盖上传后不应残留临时/备份文件: {leftovers:?}"
    );
}

/// 把文件 mtime 拨到指定时长前（模拟崩溃/取消留下的遗留产物）。
fn age_file(path: &std::path::Path, age: std::time::Duration) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    let modified = std::time::SystemTime::now() - age;
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
}

/// 文件列表接口顺手清扫当前目录：超过 1 小时的临时文件删除、目标存在的
/// 遗留备份删除、目标缺失的备份恢复成目标；新鲜临时文件保留。
#[tokio::test]
async fn file_list_sweeps_stale_upload_artifacts() {
    let app = TestServer::start().await;
    let token = app.login_token().await;
    let home = app.home();
    std::fs::create_dir_all(&home).unwrap();
    let two_hours = std::time::Duration::from_secs(2 * 3600);

    let stale_temp = home.join(format!("stale.txt.upload-{}.tmp", uuid::Uuid::new_v4()));
    let fresh_temp = home.join(format!("fresh.txt.upload-{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&stale_temp, b"x").unwrap();
    std::fs::write(&fresh_temp, b"x").unwrap();
    age_file(&stale_temp, two_hours);

    let target = home.join("backup.json");
    std::fs::write(&target, b"new").unwrap();
    let redundant = home.join(format!(
        ".backup.json.reader-upload-backup-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&redundant, b"old").unwrap();
    age_file(&redundant, two_hours);

    let missing_target = home.join("lost.txt");
    let restorable = home.join(format!(
        ".lost.txt.reader-upload-backup-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&restorable, b"old").unwrap();
    age_file(&restorable, two_hours);

    let res = app
        .client
        .get(app.url("/reader3/getWebdavFileList"))
        .query(&[("path", "/")])
        .header("Cookie", format!("reader_access={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    assert!(
        !stale_temp.exists(),
        "超过 1 小时的临时文件应被列表接口清掉"
    );
    assert!(fresh_temp.exists(), "新鲜临时文件不得清理");
    assert!(!redundant.exists(), "目标存在时遗留备份应删除");
    assert_eq!(std::fs::read(&target).unwrap(), b"new", "目标内容不得改变");
    assert!(!restorable.exists(), "目标缺失时备份应改名恢复");
    assert_eq!(
        std::fs::read(&missing_target).unwrap(),
        b"old",
        "恢复出来的目标应保留备份内容"
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

// ─────────────────────────── /v1 文件域与备份恢复 ───────────────────────────

impl TestServer {
    /// `/v1` JSON 请求：Bearer 鉴权 + CSRF 头；空 body 响应返回 Null。
    async fn v1_json(
        &self,
        token: &str,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = self
            .client
            .request(method, self.url(path))
            .bearer_auth(token)
            .header("x-requested-with", "XMLHttpRequest");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let value = if text.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
        };
        (status, value)
    }

    /// 手拼 multipart 上传（单文件）。
    async fn v1_multipart_upload(
        &self,
        token: &str,
        path: &str,
        filename: &str,
        content: &[u8],
    ) -> (StatusCode, serde_json::Value) {
        let boundary = "----webdav-v1-test";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(content);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let response = self
            .client
            .post(self.url(path))
            .bearer_auth(token)
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
            serde_json::Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
        };
        (status, value)
    }
}

#[tokio::test]
async fn v1_protocol_mount_requires_basic_auth_and_shares_home() {
    let app = TestServer::start().await;

    // 未带 Basic 凭据：与 /reader3/webdav 同一个匿名 401 质询
    let response = app
        .client
        .request(
            Method::from_bytes(b"PROPFIND").unwrap(),
            app.url("/v1/webdav/"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Basic realm=\"reader\"")
    );

    // 裸根与尾斜杠都能进入 dav-server
    for path in ["/v1/webdav", "/v1/webdav/"] {
        let response = app
            .dav("PROPFIND", path)
            .header("Depth", "0")
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::MULTI_STATUS,
            "PROPFIND {path} 应返回 207"
        );
    }

    // 协议写入落在与其他两个入口相同的用户家目录
    let response = app
        .dav("PUT", "/v1/webdav/mount.txt")
        .body("v1 mount")
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "PUT /v1/webdav/mount.txt 失败: {}",
        response.status()
    );
    assert_eq!(
        std::fs::read_to_string(app.home().join("mount.txt")).unwrap(),
        "v1 mount"
    );
    for path in ["/v1/webdav/mount.txt", "/reader3/webdav/mount.txt"] {
        let response = app.dav("GET", path).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "v1 mount");
    }
}

#[tokio::test]
async fn v1_file_browser_upload_list_download_delete() {
    let app = TestServer::start().await;
    let token = app.login_token().await;

    let (status, uploaded) = app
        .v1_multipart_upload(
            &token,
            "/v1/me/files?path=/docs",
            "a.txt",
            b"hello v1 files",
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{uploaded}");
    assert_eq!(uploaded[0]["name"], serde_json::json!("a.txt"));
    assert_eq!(uploaded[0]["path"], serde_json::json!("/docs/a.txt"));
    assert_eq!(uploaded[0]["isDirectory"], serde_json::json!(false));
    assert_eq!(uploaded[0]["size"], serde_json::json!(14));
    assert!(uploaded[0]["modifiedAt"]
        .as_str()
        .is_some_and(|value| value.ends_with('Z')));

    let response = app
        .client
        .get(app.url("/v1/me/files"))
        .bearer_auth(&token)
        .query(&[("path", "/docs")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .expect("列表必须带 ETag")
        .to_string();
    let list: serde_json::Value = response.json().await.unwrap();
    assert_eq!(list.as_array().map(Vec::len), Some(1));
    assert_eq!(list[0]["name"], serde_json::json!("a.txt"));

    let response = app
        .client
        .get(app.url("/v1/me/files"))
        .bearer_auth(&token)
        .query(&[("path", "/docs")])
        .header("if-none-match", etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(response.headers().contains_key("etag"));
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("private, no-cache")
    );

    let response = app
        .client
        .get(app.url("/v1/me/files/docs/a.txt"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    let file_etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .expect("下载必须带 ETag")
        .to_string();
    assert_eq!(response.text().await.unwrap(), "hello v1 files");

    let response = app
        .client
        .get(app.url("/v1/me/files/docs/a.txt"))
        .bearer_auth(&token)
        .header("if-none-match", file_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(response.headers().contains_key("etag"));
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("private, no-cache")
    );

    let response = app
        .client
        .delete(app.url("/v1/me/files/docs/a.txt"))
        .bearer_auth(&token)
        .header("x-requested-with", "XMLHttpRequest")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app
        .client
        .delete(app.url("/v1/me/files/docs/a.txt"))
        .bearer_auth(&token)
        .header("x-requested-with", "XMLHttpRequest")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/problem+json")));
    let problem: serde_json::Value = response.json().await.unwrap();
    assert_eq!(problem["code"], serde_json::json!("NOT_FOUND"));
}

#[tokio::test]
async fn v1_file_browser_sanitizes_paths_and_batches_delete() {
    let app = TestServer::start().await;
    let token = app.login_token().await;

    for name in ["a.txt", "b.txt"] {
        let (status, _) = app
            .v1_multipart_upload(&token, "/v1/me/files?path=/batch", name, b"x")
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let (status, problem) = app
        .v1_json(&token, Method::GET, "/v1/me/files?path=/../..", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert_eq!(problem["code"], serde_json::json!("MALFORMED_REQUEST"));

    // 通配路径里的编码回溯同样被拦
    let (status, _) = app
        .v1_json(
            &token,
            Method::GET,
            "/v1/me/files/..%2F..%2Fsecret.txt",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 批删：不存在的计入 missing
    let (status, result) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/files:batchDelete",
            Some(serde_json::json!({
                "paths": ["/batch/a.txt", "/batch/missing.txt"]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["deleted"], serde_json::json!(1));
    assert_eq!(result["missing"], serde_json::json!(1));

    // 非法路径整批拒绝，且预校验发生在删除之前：b.txt 必须还在
    let (status, _) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/files:batchDelete",
            Some(serde_json::json!({
                "paths": ["/batch/b.txt", "../evil.txt"]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(app.home().join("batch").join("b.txt").exists());

    let (status, _) = app
        .v1_json(&token, Method::DELETE, "/v1/me/files/batch", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!app.home().join("batch").exists());
}

#[tokio::test]
async fn v1_file_access_requires_session_and_sandboxes_active_content() {
    let app = TestServer::start().await;

    // 无凭据：problem+json 401
    let response = app
        .client
        .get(app.url("/v1/me/files"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let problem: serde_json::Value = response.json().await.unwrap();
    assert_eq!(problem["code"], serde_json::json!("AUTH_EXPIRED"));

    // 无凭据且缺 CSRF 头：CSRF 层先挡（403）
    let response = app
        .client
        .post(app.url("/v1/me/files"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let token = app.login_token().await;
    let (status, _) = app
        .v1_multipart_upload(
            &token,
            "/v1/me/files?path=/active",
            "page.html",
            b"<script>alert(1)</script>",
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let response = app
        .client
        .get(app.url("/v1/me/files/active/page.html"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        response
            .headers()
            .get("content-security-policy")
            .and_then(|value| value.to_str().ok()),
        Some("sandbox")
    );
}

#[tokio::test]
async fn v1_backup_create_and_restore_roundtrip() {
    let app = TestServer::start().await;
    let token = app.login_token().await;

    // 一本本地书 + 分组 + 书源 + 书签 + 替换规则
    let content = "第一章 开始\n正文一。\n第二章 中间\n正文二。\n".as_bytes();
    let (status, book) = app
        .v1_multipart_upload(&token, "/v1/local-books", "备份测试.txt", content)
        .await;
    assert_eq!(status, StatusCode::OK, "{book}");
    let book_id = book["bookId"].as_str().expect("bookId").to_string();

    let (status, _) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/groups",
            Some(serde_json::json!({
                "groupId": 2,
                "groupName": "备份组",
                "orderNo": 0
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app
        .v1_json(
            &token,
            Method::PATCH,
            &format!("/v1/me/shelf/{book_id}"),
            Some(serde_json::json!({ "group": 2 })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, result) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/sources:batchUpsert",
            Some(serde_json::json!({
                "sources": [{
                    "bookSourceName": "备份源",
                    "bookSourceUrl": "https://example.com/backup-source"
                }]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");

    let mark = serde_json::json!({
        "bookId": book_id,
        "time": 1_700_000_000_000_i64,
        "bookName": "备份测试",
        "bookAuthor": "作者",
        "chapterIndex": 0,
        "chapterPos": 0,
        "chapterName": "第一章 开始",
        "bookText": "",
        "content": "",
        "kind": "bookmark"
    });
    let (status, _) = app
        .v1_json(&token, Method::POST, "/v1/me/marks", Some(mark.clone()))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, result) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/replace-rules",
            Some(serde_json::json!({
                "name": "备份规则",
                "pattern": "a",
                "replacement": "b",
                "isEnabled": true
            })),
        )
        .await;
    assert!(status.is_success(), "{status} {result}");

    // 备份落盘（localState 由前端采集后随请求体提交）
    let (status, entry) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups",
            Some(serde_json::json!({ "localState": { "theme": "dark" } })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{entry}");
    let backup_path = entry["path"].as_str().expect("backup path").to_string();
    assert!(backup_path.starts_with("/backups/reader-backup-"));
    assert!(backup_path.ends_with(".json"));
    let on_disk = app.home().join(
        backup_path
            .trim_start_matches('/')
            .replace('/', std::path::MAIN_SEPARATOR_STR),
    );
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&on_disk).unwrap()).unwrap();
    assert_eq!(document["version"], serde_json::json!(1));
    assert_eq!(document["app"], serde_json::json!("reader-rust"));
    assert_eq!(
        document["bookshelf"]["books"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        document["bookshelf"]["groups"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(document["bookSources"].as_array().map(Vec::len), Some(1));
    assert_eq!(document["bookmarks"].as_array().map(Vec::len), Some(1));
    assert_eq!(document["replaceRules"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        document["localState"],
        serde_json::json!({ "theme": "dark" })
    );

    // 恢复前预览：条目数与本地配置标记必须与文档一致
    let (status, info) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:inspect",
            Some(serde_json::json!({ "path": backup_path })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_eq!(info["version"], serde_json::json!(1));
    assert_eq!(info["books"], serde_json::json!(1));
    assert_eq!(info["groups"], serde_json::json!(1));
    assert_eq!(info["bookSources"], serde_json::json!(1));
    assert_eq!(info["bookmarks"], serde_json::json!(1));
    assert_eq!(info["replaceRules"], serde_json::json!(1));
    assert_eq!(info["hasLocalState"], serde_json::json!(true));

    // 清空本地状态，制造「恢复确实重建了数据」的落差
    for (path, body) in [
        (
            "/v1/me/shelf:batchDelete",
            serde_json::json!({ "bookIds": [book_id] }),
        ),
        (
            "/v1/me/marks:batchDelete",
            serde_json::json!([mark.clone()]),
        ),
        (
            "/v1/me/replace-rules:batchDelete",
            serde_json::json!({ "names": ["备份规则"] }),
        ),
        (
            "/v1/sources",
            serde_json::json!({ "confirm": "DELETE_ALL" }),
        ),
    ] {
        let method = if path == "/v1/sources" {
            Method::DELETE
        } else {
            Method::POST
        };
        let (status, result) = app.v1_json(&token, method, path, Some(body)).await;
        assert!(status.is_success(), "{path} 清理失败: {status} {result}");
    }
    let (status, _) = app
        .v1_json(&token, Method::DELETE, "/v1/me/groups/2", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, restored) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:restore",
            Some(serde_json::json!({ "path": backup_path })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["books"], serde_json::json!(1));
    assert_eq!(restored["groups"], serde_json::json!(1));
    assert_eq!(restored["bookSources"], serde_json::json!(1));
    assert_eq!(restored["bookmarks"], serde_json::json!(1));
    assert_eq!(restored["skippedBookmarks"], serde_json::json!(0));
    assert_eq!(restored["replaceRules"], serde_json::json!(1));
    assert_eq!(
        restored["localState"],
        serde_json::json!({ "theme": "dark" })
    );

    // 各域都已重建，书籍分组位也保留
    let (_, groups) = app
        .v1_json(&token, Method::GET, "/v1/me/groups", None)
        .await;
    assert_eq!(groups[0]["groupId"], serde_json::json!(2));
    let (_, shelf) = app
        .v1_json(&token, Method::GET, "/v1/me/shelf?limit=100", None)
        .await;
    assert_eq!(shelf["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(shelf["items"][0]["group"], serde_json::json!(2));
    let (_, marks) = app.v1_json(&token, Method::GET, "/v1/me/marks", None).await;
    assert_eq!(marks.as_array().map(Vec::len), Some(1));
    let (_, rules) = app
        .v1_json(&token, Method::GET, "/v1/me/replace-rules", None)
        .await;
    assert_eq!(rules.as_array().map(Vec::len), Some(1));
    assert_eq!(rules[0]["name"], serde_json::json!("备份规则"));
    let (_, sources) = app.v1_json(&token, Method::GET, "/v1/sources", None).await;
    assert_eq!(sources["items"][0]["name"], serde_json::json!("备份源"));
}

#[tokio::test]
async fn v1_backup_restore_validates_filters_and_reports_skips() {
    let app = TestServer::start().await;
    let token = app.login_token().await;
    let backups = app.home().join("backups");
    std::fs::create_dir_all(&backups).unwrap();

    // 合法文档：非法分组 id=3 应被丢弃，书签指向未登记书籍应计 skipped，
    // localState 原样返回
    let document = serde_json::json!({
        "version": 1,
        "createdAt": "2026-11-14T00:00:00Z",
        "app": "reader-rust-frontend",
        "bookshelf": {
            "books": [],
            "groups": [
                { "groupId": 3, "groupName": "非法位", "orderNo": 0 },
                { "groupId": 2, "groupName": "合法组", "orderNo": 1 }
            ]
        },
        "bookSources": [],
        "bookmarks": [{
            "bookId": "book_999999",
            "time": 1,
            "bookName": "不存在",
            "bookAuthor": "",
            "chapterIndex": 0,
            "chapterPos": 0,
            "chapterName": "",
            "bookText": "",
            "content": "",
            "kind": "bookmark"
        }],
        "replaceRules": [{
            "id": 0,
            "name": "带 空格 的旧规则",
            "pattern": "x",
            "replacement": "y",
            "isEnabled": true,
            "isRegex": false,
            "order": 0
        }],
        "localState": { "theme": "dark" }
    });
    std::fs::write(
        backups.join("crafted.json"),
        serde_json::to_vec_pretty(&document).unwrap(),
    )
    .unwrap();

    let (status, result) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:restore",
            Some(serde_json::json!({ "path": "/backups/crafted.json" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["groups"], serde_json::json!(1));
    assert_eq!(result["bookmarks"], serde_json::json!(0));
    assert_eq!(result["skippedBookmarks"], serde_json::json!(1));
    assert_eq!(result["replaceRules"], serde_json::json!(1));
    assert_eq!(result["localState"]["theme"], serde_json::json!("dark"));

    let (_, groups) = app
        .v1_json(&token, Method::GET, "/v1/me/groups", None)
        .await;
    assert_eq!(groups.as_array().map(Vec::len), Some(1));
    assert_eq!(groups[0]["groupId"], serde_json::json!(2));
    let (_, rules) = app
        .v1_json(&token, Method::GET, "/v1/me/replace-rules", None)
        .await;
    assert_eq!(rules[0]["name"], serde_json::json!("带 空格 的旧规则"));

    // 非法版本 / 非法 JSON / 路径不存在 / 路径回溯
    std::fs::write(
        backups.join("bad-version.json"),
        br#"{"version":2,"createdAt":"","app":"","bookshelf":{"books":[],"groups":[]},"bookSources":[],"bookmarks":[],"replaceRules":[]}"#,
    )
    .unwrap();
    std::fs::write(backups.join("bad-json.json"), b"not json").unwrap();

    for (path, expected) in [
        ("/backups/bad-version.json", StatusCode::BAD_REQUEST),
        ("/backups/bad-json.json", StatusCode::UNPROCESSABLE_ENTITY),
        ("/backups/missing.json", StatusCode::NOT_FOUND),
        ("../reader.db", StatusCode::BAD_REQUEST),
    ] {
        let (status, problem) = app
            .v1_json(
                &token,
                Method::POST,
                "/v1/me/backups:restore",
                Some(serde_json::json!({ "path": path })),
            )
            .await;
        assert_eq!(status, expected, "{path}: {problem}");
    }

    // 结构不完整的文档必须 422，且不能动现有数据（一个 {"version":1}
    // 就能清空全部域是旧实现最危险的回归）
    let (status, _) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/groups",
            Some(serde_json::json!({
                "groupId": 4,
                "groupName": "保留组",
                "orderNo": 2
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    std::fs::write(backups.join("incomplete.json"), br#"{"version":1}"#).unwrap();
    let (status, problem) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:restore",
            Some(serde_json::json!({ "path": "/backups/incomplete.json" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");

    // 写库前预校验：非法 bookId / 书签 chapterPos 都应在清空任何域之前拒绝
    std::fs::write(
        backups.join("bad-book-id.json"),
        br#"{"version":1,"createdAt":"","app":"","bookshelf":{"books":[{"bookUrl":"https://example.com/x","bookId":"book_bad"}],"groups":[]},"bookSources":[],"bookmarks":[],"replaceRules":[]}"#,
    )
    .unwrap();
    std::fs::write(
        backups.join("bad-mark-pos.json"),
        br#"{"version":1,"createdAt":"","app":"","bookshelf":{"books":[],"groups":[]},"bookSources":[],"bookmarks":[{"bookId":"book_1","chapterIndex":0,"chapterPos":20000,"kind":"bookmark"}],"replaceRules":[]}"#,
    )
    .unwrap();
    for path in ["/backups/bad-book-id.json", "/backups/bad-mark-pos.json"] {
        let (status, problem) = app
            .v1_json(
                &token,
                Method::POST,
                "/v1/me/backups:restore",
                Some(serde_json::json!({ "path": path })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {problem}");
    }
    let (_, groups) = app
        .v1_json(&token, Method::GET, "/v1/me/groups", None)
        .await;
    assert!(
        groups
            .as_array()
            .is_some_and(|items| items.iter().any(|group| group["groupId"] == 2)),
        "预校验失败后原分组必须保留: {groups}"
    );
    assert!(
        groups
            .as_array()
            .is_some_and(|items| items.iter().any(|group| group["groupId"] == 4)),
        "结构不完整文档不得删除现有分组: {groups}"
    );
}

#[tokio::test]
async fn v1_backup_restore_remaps_conflicting_book_ids() {
    let app = TestServer::start().await;
    let token = app.login_token().await;

    // 先占住 book_1（本地书），备份里的另一本书也声明 book_1：恢复时必须
    // 给新书分配新 id，并把书签与规则 scope 一起重写，而不是静默丢书签
    let (status, local_book) = app
        .v1_multipart_upload(
            &token,
            "/v1/local-books",
            "占位书.txt",
            "第一章 开始\n正文。\n".as_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{local_book}");
    assert_eq!(local_book["bookId"], serde_json::json!("book_1"));

    let backups = app.home().join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    let document = serde_json::json!({
        "version": 1,
        "createdAt": "2026-11-14T00:00:00Z",
        "app": "reader-rust",
        "bookshelf": {
            "books": [{
                "bookId": "book_1",
                "bookUrl": "https://example.com/conflicting-book",
                "name": "冲突书",
                "author": "作者"
            }],
            "groups": []
        },
        "bookSources": [],
        "bookmarks": [{
            "bookId": "book_1",
            "time": 1,
            "bookName": "冲突书",
            "bookAuthor": "作者",
            "chapterIndex": 0,
            "chapterPos": 0,
            "chapterName": "第一章",
            "bookText": "",
            "content": "",
            "kind": "bookmark"
        }],
        "replaceRules": [{
            "id": 0,
            "name": "映射规则",
            "pattern": "a",
            "replacement": "b",
            "scope": "book:book_1",
            "isEnabled": true,
            "isRegex": false,
            "order": 0
        }],
        "localState": {}
    });
    std::fs::write(
        backups.join("conflict.json"),
        serde_json::to_vec_pretty(&document).unwrap(),
    )
    .unwrap();

    let (status, result) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:restore",
            Some(serde_json::json!({ "path": "/backups/conflict.json" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["bookmarks"], serde_json::json!(1));
    assert_eq!(result["skippedBookmarks"], serde_json::json!(0));

    let (_, shelf) = app
        .v1_json(&token, Method::GET, "/v1/me/shelf?limit=100", None)
        .await;
    let restored_id = shelf["items"][0]["bookId"]
        .as_str()
        .expect("restored bookId")
        .to_string();
    assert_ne!(restored_id, "book_1", "book_1 已被本地书占用，必须重分配");
    let (_, marks) = app.v1_json(&token, Method::GET, "/v1/me/marks", None).await;
    assert_eq!(marks[0]["bookId"], serde_json::json!(restored_id));
    let (_, rules) = app
        .v1_json(&token, Method::GET, "/v1/me/replace-rules", None)
        .await;
    assert_eq!(
        rules[0]["scope"],
        serde_json::json!(format!("book:{restored_id}"))
    );
}

#[tokio::test]
async fn v1_backup_without_local_state_returns_null() {
    let app = TestServer::start().await;
    let token = app.login_token().await;
    let backups = app.home().join("backups");
    std::fs::create_dir_all(&backups).unwrap();

    // 创建请求省略 localState：文档里应为 null，而不是空对象
    let (status, entry) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups",
            Some(serde_json::json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{entry}");
    let on_disk = app.home().join(
        entry["path"]
            .as_str()
            .unwrap()
            .trim_start_matches('/')
            .replace('/', std::path::MAIN_SEPARATOR_STR),
    );
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&on_disk).unwrap()).unwrap();
    assert_eq!(document["localState"], serde_json::Value::Null);

    // 旧备份/手写文档可能没有 localState 字段：返回 null，前端据此不动本地键
    std::fs::write(
        backups.join("no-local-state.json"),
        br#"{"version":1,"bookshelf":{"books":[],"groups":[]},"bookSources":[],"bookmarks":[],"replaceRules":[]}"#,
    )
    .unwrap();
    let (status, info) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:inspect",
            Some(serde_json::json!({ "path": "/backups/no-local-state.json" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_eq!(info["hasLocalState"], serde_json::json!(false));
    let (status, result) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:restore",
            Some(serde_json::json!({ "path": "/backups/no-local-state.json" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["localState"], serde_json::Value::Null);
}

#[tokio::test]
async fn v1_file_and_restore_error_paths() {
    let app = TestServer::start().await;
    let token = app.login_token().await;

    // multipart 文件名穿越：文件名不是纯文件名，整请求 400
    let (status, problem) = app
        .v1_multipart_upload(&token, "/v1/me/files", "../evil.txt", b"x")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");

    // 目录不存在 404；对文件路径调列表 400
    let (status, problem) = app
        .v1_json(&token, Method::GET, "/v1/me/files?path=/missing", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["code"], serde_json::json!("NOT_FOUND"));

    let (status, _) = app
        .v1_multipart_upload(&token, "/v1/me/files?path=/docs", "a.txt", b"x")
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = app
        .v1_json(&token, Method::GET, "/v1/me/files?path=/docs/a.txt", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 下载目录按不存在处理；空批删 400
    let (status, _) = app
        .v1_json(&token, Method::GET, "/v1/me/files/docs", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/files:batchDelete",
            Some(serde_json::json!({ "paths": [] })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 恢复写库前拒绝：缺 bookUrl / 非法书签 kind
    let backups = app.home().join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    std::fs::write(
        backups.join("no-book-url.json"),
        br#"{"version":1,"bookshelf":{"books":[{"bookId":"book_1"}],"groups":[]},"bookSources":[],"bookmarks":[],"replaceRules":[]}"#,
    )
    .unwrap();
    std::fs::write(
        backups.join("bad-mark-kind.json"),
        br#"{"version":1,"bookshelf":{"books":[],"groups":[]},"bookSources":[],"bookmarks":[{"bookId":"book_1","chapterIndex":0,"chapterPos":0,"kind":"bad"}],"replaceRules":[]}"#,
    )
    .unwrap();
    for path in ["/backups/no-book-url.json", "/backups/bad-mark-kind.json"] {
        let (status, problem) = app
            .v1_json(
                &token,
                Method::POST,
                "/v1/me/backups:restore",
                Some(serde_json::json!({ "path": path })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {problem}");
    }

    // 嵌套空实体 / 控制字符 URL 同样必须在删除前拒绝
    let (status, _) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/groups",
            Some(serde_json::json!({
                "groupId": 8,
                "groupName": "守卫组",
                "orderNo": 3
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let invalid_documents = [
        (
            "empty-source.json",
            r#"{"version":1,"bookshelf":{"books":[],"groups":[]},"bookSources":[{}],"bookmarks":[],"replaceRules":[]}"#,
        ),
        (
            "empty-rule.json",
            r#"{"version":1,"bookshelf":{"books":[],"groups":[]},"bookSources":[],"bookmarks":[],"replaceRules":[{}]}"#,
        ),
        (
            "empty-mark.json",
            r#"{"version":1,"bookshelf":{"books":[],"groups":[]},"bookSources":[],"bookmarks":[{}],"replaceRules":[]}"#,
        ),
        (
            "control-url.json",
            r#"{"version":1,"bookshelf":{"books":[{"bookUrl":"\u0001","bookId":"book_1"}],"groups":[]},"bookSources":[],"bookmarks":[],"replaceRules":[]}"#,
        ),
    ];
    for (name, body) in invalid_documents {
        std::fs::write(backups.join(name), body).unwrap();
        let (status, problem) = app
            .v1_json(
                &token,
                Method::POST,
                "/v1/me/backups:restore",
                Some(serde_json::json!({ "path": format!("/backups/{name}") })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {problem}");
    }
    let (_, groups) = app
        .v1_json(&token, Method::GET, "/v1/me/groups", None)
        .await;
    assert!(
        groups.as_array().is_some_and(|items| items
            .iter()
            .any(|group| group["groupId"] == serde_json::json!(8))),
        "嵌套实体校验失败后原分组必须保留: {groups}"
    );

    // 备份文件超过 100MB：先看元数据再读，不整读超大文件
    let huge = backups.join("huge.json");
    let file = std::fs::File::create(&huge).unwrap();
    file.set_len(100 * 1024 * 1024 + 1).unwrap();
    drop(file);
    let (status, problem) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:restore",
            Some(serde_json::json!({ "path": "/backups/huge.json" })),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{problem}");
}

#[tokio::test]
async fn v1_backup_restore_skips_bookmarks_outside_backup_shelf() {
    let app = TestServer::start().await;
    let token = app.login_token().await;

    // book_1 在目标库已登记，但不在备份书架里：书签不能按旧 id 挂到它身上
    let (status, book) = app
        .v1_multipart_upload(
            &token,
            "/v1/local-books",
            "旧书.txt",
            "第一章 开始\n正文。\n".as_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{book}");
    assert_eq!(book["bookId"], serde_json::json!("book_1"));

    let backups = app.home().join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    std::fs::write(
        backups.join("off-shelf-mark.json"),
        br#"{"version":1,"bookshelf":{"books":[],"groups":[]},"bookSources":[],"bookmarks":[{"bookId":"book_1","time":1,"chapterIndex":0,"chapterPos":0,"kind":"bookmark"}],"replaceRules":[]}"#,
    )
    .unwrap();
    let (status, result) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:restore",
            Some(serde_json::json!({ "path": "/backups/off-shelf-mark.json" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["bookmarks"], serde_json::json!(0));
    assert_eq!(result["skippedBookmarks"], serde_json::json!(1));

    let (_, marks) = app.v1_json(&token, Method::GET, "/v1/me/marks", None).await;
    assert_eq!(marks.as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn v1_backup_preserves_bookmarks_for_off_shelf_books() {
    let app = TestServer::start().await;
    let token = app.login_token().await;

    let (status, book) = app
        .v1_multipart_upload(
            &token,
            "/v1/local-books",
            "书架外.txt",
            "第一章 开始\n正文。\n".as_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{book}");
    let book_id = book["bookId"].as_str().unwrap().to_string();

    let mark = serde_json::json!({
        "bookId": book_id,
        "time": 1,
        "bookName": "书架外",
        "bookAuthor": "",
        "chapterIndex": 0,
        "chapterPos": 0,
        "chapterName": "第一章 开始",
        "bookText": "",
        "content": "",
        "kind": "bookmark"
    });
    let (status, _) = app
        .v1_json(&token, Method::POST, "/v1/me/marks", Some(mark))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // 移出书架：登记行与书签都保留，快照必须给这条书签补 bookUrl
    let (status, _) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/shelf:batchDelete",
            Some(serde_json::json!({ "bookIds": [book_id] })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, entry) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups",
            Some(serde_json::json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{entry}");
    let on_disk = app.home().join(
        entry["path"]
            .as_str()
            .unwrap()
            .trim_start_matches('/')
            .replace('/', std::path::MAIN_SEPARATOR_STR),
    );
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&on_disk).unwrap()).unwrap();
    assert!(
        document["bookmarks"][0]["bookUrl"]
            .as_str()
            .is_some_and(|url| !url.is_empty()),
        "书架外书签必须带 bookUrl: {}",
        document["bookmarks"][0]
    );

    let (status, result) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:restore",
            Some(serde_json::json!({ "path": entry["path"] })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["bookmarks"], serde_json::json!(1));
    assert_eq!(result["skippedBookmarks"], serde_json::json!(0));
    let (_, marks) = app.v1_json(&token, Method::GET, "/v1/me/marks", None).await;
    assert_eq!(marks.as_array().map(Vec::len), Some(1));
    assert_eq!(marks[0]["bookId"], serde_json::json!(book_id));
}

#[tokio::test]
async fn v1_backup_restore_rebinds_books_to_restored_sources() {
    let app = TestServer::start().await;
    let token = app.login_token().await;
    let backups = app.home().join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    std::fs::write(
        backups.join("source-bind.json"),
        br#"{"version":1,"bookshelf":{"books":[{"bookId":"book_10","bookUrl":"https://example.com/b","bookSourceId":5,"name":"bound-book","author":""}],"groups":[]},"bookSources":[{"bookSourceId":5,"bookSourceName":"bound-source","bookSourceUrl":"https://example.com/s"}],"bookmarks":[],"replaceRules":[]}"#,
    )
    .unwrap();
    let (status, result) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:restore",
            Some(serde_json::json!({ "path": "/backups/source-bind.json" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["books"], serde_json::json!(1));
    assert_eq!(result["bookSources"], serde_json::json!(1));

    let (_, shelf) = app
        .v1_json(&token, Method::GET, "/v1/me/shelf?limit=100", None)
        .await;
    assert_eq!(shelf["items"][0]["bookId"], serde_json::json!("book_10"));
    assert_eq!(shelf["items"][0]["sourceId"], serde_json::json!("src_5"));
}

#[tokio::test]
async fn v1_backup_restore_is_atomic_on_midway_failure() {
    let app = TestServer::start().await;
    let token = app.login_token().await;

    // 先建立一套完整旧状态
    let (status, book) = app
        .v1_multipart_upload(
            &token,
            "/v1/local-books",
            "原子旧书.txt",
            "第一章 开始\n正文。\n".as_bytes(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{book}");
    let book_id = book["bookId"].as_str().unwrap().to_string();
    let mark = serde_json::json!({
        "bookId": book_id,
        "time": 1,
        "bookName": "原子旧书",
        "bookAuthor": "",
        "chapterIndex": 0,
        "chapterPos": 0,
        "chapterName": "第一章 开始",
        "bookText": "",
        "content": "",
        "kind": "bookmark"
    });
    let (status, _) = app
        .v1_json(&token, Method::POST, "/v1/me/marks", Some(mark))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/groups",
            Some(serde_json::json!({
                "groupId": 2,
                "groupName": "旧组",
                "orderNo": 0
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/sources:batchUpsert",
            Some(serde_json::json!({
                "sources": [{
                    "bookSourceName": "旧源",
                    "bookSourceUrl": "https://example.com/old-source"
                }]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/replace-rules",
            Some(serde_json::json!({
                "name": "旧规则",
                "pattern": "a",
                "replacement": "b",
                "isEnabled": true
            })),
        )
        .await;
    assert!(status.is_success(), "{status}");

    // 在最后一个域（replace_rules）注入 DB 错误，让 restore 走到第⑤步才失败
    let pool = sqlx::SqlitePool::connect(&format!(
        "sqlite:{}",
        app.temp_dir.join("reader.db").display()
    ))
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_restore_replace_rules BEFORE INSERT ON replace_rules \
         BEGIN SELECT RAISE(ABORT, 'injected restore failure'); END",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let backups = app.home().join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    std::fs::write(
        backups.join("atomic.json"),
        br#"{"version":1,"bookshelf":{"books":[{"bookId":"book_10","bookUrl":"https://example.com/new-book","bookSourceId":9,"name":"new-book","author":""}],"groups":[{"groupId":4,"groupName":"new-group","orderNo":0}]},"bookSources":[{"bookSourceId":9,"bookSourceName":"new-source","bookSourceUrl":"https://example.com/new-source"}],"bookmarks":[],"replaceRules":[{"name":"new-rule","pattern":"x","replacement":"y","isEnabled":true}]}"#,
    )
    .unwrap();
    let (status, problem) = app
        .v1_json(
            &token,
            Method::POST,
            "/v1/me/backups:restore",
            Some(serde_json::json!({ "path": "/backups/atomic.json" })),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{problem}");

    // 五个域必须全部保持旧状态（事务整体回滚）
    let (_, sources) = app.v1_json(&token, Method::GET, "/v1/sources", None).await;
    assert_eq!(sources["items"][0]["name"], serde_json::json!("旧源"));
    assert_eq!(sources["items"].as_array().map(Vec::len), Some(1));
    let (_, groups) = app
        .v1_json(&token, Method::GET, "/v1/me/groups", None)
        .await;
    assert_eq!(groups[0]["groupId"], serde_json::json!(2));
    assert_eq!(groups.as_array().map(Vec::len), Some(1));
    let (_, shelf) = app
        .v1_json(&token, Method::GET, "/v1/me/shelf?limit=100", None)
        .await;
    assert_eq!(shelf["items"][0]["bookId"], serde_json::json!(book_id));
    let (_, marks) = app.v1_json(&token, Method::GET, "/v1/me/marks", None).await;
    assert_eq!(marks.as_array().map(Vec::len), Some(1));
    let (_, rules) = app
        .v1_json(&token, Method::GET, "/v1/me/replace-rules", None)
        .await;
    assert_eq!(rules[0]["name"], serde_json::json!("旧规则"));
    assert_eq!(rules.as_array().map(Vec::len), Some(1));
}
