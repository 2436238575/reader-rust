//! 安全响应头与上传类型白名单的端到端行为。
//!
//! 起真实监听端口的服务，覆盖：全局 nosniff/Referrer-Policy/X-Frame-Options、
//! 首页 HTML 的 CSP、/assets 的 sandbox CSP、uploadFile 的位图扩展名白名单、
//! EPUB SVG 资产的沙箱 CSP，以及 aiProxyImage 对 SVG 上游的拒绝
//! （上游用本地假服务，不触真实网络）。

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
            std::env::temp_dir().join(format!("reader-rust-sec-{}", uuid::Uuid::new_v4()));
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
            // aiProxyImage 的上游是本测试起的本地假服务
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
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        body["data"]["accessToken"].as_str().unwrap().to_string()
    }

    /// 手工拼 multipart body（与 auth_flow 的既有做法一致，避免引入新依赖）。
    fn multipart_body(parts: &[(&str, &str, &str, Vec<u8>)]) -> (String, Vec<u8>) {
        let boundary = "----secboundary";
        let mut body: Vec<u8> = Vec::new();
        for (name, filename, content_type, content) in parts {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(content);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        (format!("multipart/form-data; boundary={boundary}"), body)
    }

    async fn upload(
        &self,
        path: &str,
        token: &str,
        parts: &[(&str, &str, &str, Vec<u8>)],
    ) -> reqwest::Response {
        let (content_type, body) = Self::multipart_body(parts);
        self.client
            .post(self.url(path))
            .bearer_auth(token)
            .header("Content-Type", content_type)
            .body(body)
            .send()
            .await
            .unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        common::remove_dir_retried(&self.temp_dir);
    }
}

/// 本地假上游：/svg 回 SVG，/png 回 PNG（供 aiProxyImage 测试，无真实网络）。
async fn start_fake_image_upstream() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/{kind}",
            axum::routing::get(
                |axum::extract::Path(kind): axum::extract::Path<String>| async move {
                    match kind.as_str() {
                        "svg" => (
                            [(reqwest::header::CONTENT_TYPE, "image/svg+xml")],
                            "<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\"/>"
                                .as_bytes()
                                .to_vec(),
                        ),
                        _ => (
                            [(reqwest::header::CONTENT_TYPE, "image/png")],
                            vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
                        ),
                    }
                },
            ),
        );
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn epub_fixture_with_svg() -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let files: &[(&str, &[u8])] = &[
            ("mimetype", "application/epub+zip".as_bytes()),
            (
                "META-INF/container.xml",
                r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles>
</container>"#
                    .as_bytes(),
            ),
            (
                "OEBPS/content.opf",
                r#"<package version="3.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>SVG EPUB</dc:title><dc:creator>author</dc:creator>
  </metadata>
  <manifest>
    <item id="ch1" href="Text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="art" href="Images/art.svg" media-type="image/svg+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"#
                    .as_bytes(),
            ),
            (
                "OEBPS/Text/ch1.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>chapter body</p>
<img src="../Images/art.svg" alt="illustration"/></body></html>"#
                    .as_bytes(),
            ),
            (
                "OEBPS/Images/art.svg",
                b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\"><script>alert(2)</script></svg>",
            ),
        ];
        for (path, content) in files {
            zip.start_file(*path, options).unwrap();
            zip.write_all(content).unwrap();
        }
        zip.finish().unwrap();
    }
    cursor.into_inner()
}

#[tokio::test]
async fn index_html_gets_csp_and_global_security_headers() {
    let server = TestServer::start().await;

    // `/` 走显式 ServeFile，`/index.html` 走 ServeDir fallback，两处都要带 CSP
    for path in ["/", "/index.html"] {
        let response = server.client.get(server.url(path)).send().await.unwrap();
        assert_eq!(response.status(), 200, "{path} 应可取");
        let headers = response.headers();
        assert_eq!(
            headers.get("x-content-type-options").unwrap(),
            "nosniff",
            "{path} 应带 nosniff"
        );
        assert_eq!(headers.get("referrer-policy").unwrap(), "same-origin");
        // SAMEORIGIN 而不是 DENY：SourcesView 用同源 iframe 打开 bookSourceProxy
        assert_eq!(headers.get("x-frame-options").unwrap(), "SAMEORIGIN");
        let csp = headers
            .get("content-security-policy")
            .unwrap_or_else(|| panic!("{path} 应带 HTML CSP"))
            .to_str()
            .unwrap();
        assert!(csp.contains("default-src 'self'"), "{path} CSP: {csp}");
        assert!(csp.contains("script-src 'self'"), "{path} CSP: {csp}");
        assert!(csp.contains("object-src 'none'"), "{path} CSP: {csp}");
    }
}

#[tokio::test]
async fn assets_responses_carry_nosniff_and_sandbox_csp() {
    let server = TestServer::start().await;
    let assets_dir = server.temp_dir.join("assets");
    std::fs::create_dir_all(&assets_dir).unwrap();
    std::fs::write(assets_dir.join("probe.js"), b"console.log(1)").unwrap();
    std::fs::write(assets_dir.join("probe.svg"), b"<svg/>").unwrap();
    // HTML 资产也要保持 sandbox：外层的 HTML CSP 中间件是 if_not_present，
    // 不能把 sandbox 升级成主站 CSP
    std::fs::write(assets_dir.join("probe.html"), b"<html></html>").unwrap();

    for name in ["probe.js", "probe.svg", "probe.html"] {
        let response = server
            .client
            .get(server.url(&format!("/assets/{name}")))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{name} 应可取");
        let headers = response.headers();
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        let csp = headers.get("content-security-policy").unwrap();
        assert_eq!(csp, "sandbox", "{name} 的 assets CSP 应是纯 sandbox");
    }
}

#[tokio::test]
async fn upload_file_rejects_non_bitmap_extensions() {
    let server = TestServer::start().await;
    let token = server.login().await;

    // SVG 与主站同源直出会是脚本落脚点：400
    let response = server
        .upload(
            "/reader3/uploadFile?type=images",
            &token,
            &[(
                "file",
                "evil.svg",
                "image/svg+xml",
                b"<svg onload=\"alert(1)\"/>".to_vec(),
            )],
        )
        .await;
    assert_eq!(response.status(), 400, "SVG 上传应被拒绝");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(false));
    assert!(body["errorMsg"].as_str().unwrap().contains("图片"));

    // 位图放行，大小写不敏感
    let response = server
        .upload(
            "/reader3/uploadFile?type=images",
            &token,
            &[(
                "file",
                "pic.PNG",
                "image/png",
                vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
            )],
        )
        .await;
    assert_eq!(response.status(), 200, "PNG 上传应成功");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(true));
    let urls = body["data"].as_array().unwrap();
    assert_eq!(urls.len(), 1);
}

#[tokio::test]
async fn epub_svg_asset_is_served_with_sandbox_csp() {
    let server = TestServer::start().await;
    let token = server.login().await;

    let response = server
        .upload(
            "/reader3/uploadEpubBook",
            &token,
            &[(
                "file",
                "art.epub",
                "application/epub+zip",
                epub_fixture_with_svg(),
            )],
        )
        .await;
    assert_eq!(response.status(), 200, "EPUB 导入应成功");
    let body: serde_json::Value = response.json().await.unwrap();
    let book_id = body["data"]["bookId"]
        .as_str()
        .expect("响应应含 bookId")
        .to_string();

    let response = server
        .client
        .get(server.url(&format!(
            "/reader3/localEpubAsset?bookId={book_id}&path=OEBPS/Images/art.svg"
        )))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "image/svg+xml"
    );
    // 沙箱 CSP：直接打开时不执行内嵌脚本，但 <img> 渲染与内联样式保留
    assert_eq!(
        response.headers().get("content-security-policy").unwrap(),
        "default-src 'none'; style-src 'unsafe-inline'"
    );
}

#[tokio::test]
async fn ai_proxy_image_rejects_svg_upstream() {
    let server = TestServer::start().await;
    let token = server.login().await;
    let upstream = start_fake_image_upstream().await;

    // SVG 上游：可携带脚本，不能透传
    let response = server
        .client
        .post(server.url("/reader3/aiProxyImage"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "url": format!("{upstream}/svg") }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400, "SVG 上游应被拒绝");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], serde_json::json!(false));

    // 位图上游正常透传
    let response = server
        .client
        .post(server.url("/reader3/aiProxyImage"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "url": format!("{upstream}/png") }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "PNG 上游应透传");
    assert_eq!(response.headers().get("content-type").unwrap(), "image/png");
}

/// 压缩层挂在最外层 Router 上：/reader3 的 JSON 与静态资源共用一层。
/// reqwest 默认自动解压并剥掉 content-encoding 头，这里用 no_gzip 客户端
/// 才能看到线上真实的响应头。
#[tokio::test]
async fn api_json_responses_are_gzip_compressed_when_accepted() {
    let server = TestServer::start().await;
    let client = reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .unwrap();

    // /health 响应约 70 字节，超过 DefaultPredicate 的 32 字节下限
    let response = client
        .get(server.url("/health"))
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-encoding").unwrap(),
        "gzip",
        "声明接受 gzip 时 /reader3 的 JSON 应被压缩"
    );
    let body = response.bytes().await.unwrap();
    assert_eq!(
        &body[..2],
        &[0x1f, 0x8b],
        "响应体应是 gzip 魔数开头的压缩流"
    );

    // 不声明 accept-encoding 时不压缩
    let response = client.get(server.url("/health")).send().await.unwrap();
    assert!(response.headers().get("content-encoding").is_none());
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["data"]["status"], serde_json::json!("ok"));

    // SSE（text/event-stream）被 tower-http 的 DefaultPredicate 按 content-type
    // 排除（还有 gRPC、image/* 与 <32 字节的响应），压缩层对其走 Identity 透传、
    // 不缓冲事件流；起真实 SSE 流要触发抓取，集成成本过高，此处不覆断言。
}
