//! AI 资料编排任务流：假 sidecar（tests/helpers/fake_agent_sidecar.mjs）
//! 全流程覆盖——批量更新、章节跳过、工具回调（正文获取/搜索钳制）、
//! 地图生成与降级、看门狗超时与取消、并发 409。
//!
//! 前置：测试机需要有 Node（跑假 sidecar）。

mod common;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use reader_rust::api::router::build_router;
use reader_rust::app::bootstrap::build_state;
use reader_rust::app::config::AppConfig;
use serde_json::{json, Value};

/** 假 sidecar 是 Node 脚本；无 Node 环境时相关用例跳过而非硬失败。 */
fn node_available() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok()
    })
}

const TEST_USERNAME: &str = "reader1";
const TEST_PASSWORD: &str = "password123";
const FAKE_SIDECAR: &str = "tests/helpers/fake_agent_sidecar.mjs";

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
    async fn start(chapter_timeout_secs: u64) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_test_writer()
            .try_init();
        common::clean_stale_temp_dirs();
        let temp_dir =
            std::env::temp_dir().join(format!("reader-rust-agent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let web_root = temp_dir.join("dist");
        std::fs::create_dir_all(&web_root).unwrap();
        std::fs::write(
            web_root.join("index.html"),
            "<!doctype html><title>app</title>",
        )
        .unwrap();

        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fake_script = manifest_dir.join(FAKE_SIDECAR);

        // 模型就绪检查需要 AI_TEXT_* 配置；假 sidecar 不会真的调用模型，
        // 指向一个不可达地址即可
        std::env::set_var("AI_TEXT_ENABLED", "true");
        std::env::set_var("AI_TEXT_BASE_URL", "http://127.0.0.1:9");
        std::env::set_var("AI_TEXT_MODEL", "test-model");
        std::env::set_var("AI_IMAGE_ENABLED", "true");
        std::env::set_var("AI_IMAGE_BASE_URL", "http://127.0.0.1:9");
        std::env::set_var("AI_IMAGE_MODEL", "test-image");

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
            // 假上游监听 127.0.0.1：显式放行私网出站（默认拦截）
            allow_private_network: true,
            agent_sidecar_command: format!("node \"{}\"", fake_script.display()),
            agent_sidecar_chapter_timeout_secs: chapter_timeout_secs,
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
            .json(&json!({"username": TEST_USERNAME, "password": TEST_PASSWORD}))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "login failed: {}",
            response.status()
        );
        let body: Value = response.json().await.unwrap();
        body["data"]["accessToken"].as_str().unwrap().to_string()
    }

    /// 上传本地 TXT（multipart 手工构造），返回 (bookId, bookUrl)。
    async fn upload_txt(&self, token: &str, file_name: &str, content: &str) -> (String, String) {
        let boundary = "----agenttestboundary";
        let body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             {content}\r\n\
             --{boundary}--\r\n"
        );
        let response = self
            .client
            .post(self.url("/reader3/uploadTxtBook"))
            .header("Authorization", format!("Bearer {token}"))
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "upload failed: {}",
            response.status()
        );
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["isSuccess"], json!(true), "upload error: {}", body);
        (
            body["data"]["bookId"].as_str().unwrap().to_string(),
            body["data"]["bookUrl"].as_str().unwrap().to_string(),
        )
    }

    /// 上传后用 saveBook 改作者（redraw 测试以此分流假 sidecar 行为）。
    async fn upload_txt_as(
        &self,
        token: &str,
        file_name: &str,
        content: &str,
        author: &str,
    ) -> String {
        let (book_id, book_url) = self.upload_txt(token, file_name, content).await;
        let response = self
            .client
            .post(self.url("/reader3/saveBook"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&json!({"bookId": book_id, "bookUrl": book_url, "author": author}))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "saveBook failed");
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["isSuccess"], json!(true), "saveBook error: {}", body);
        book_id
    }

    async fn start_task(
        &self,
        token: &str,
        book_id: &str,
        kind: &str,
        target: Option<i32>,
    ) -> reqwest::Response {
        self.client
            .post(self.url("/reader3/runAgentTask"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&json!({"bookId": book_id, "kind": kind, "targetChapterIndex": target}))
            .send()
            .await
            .unwrap()
    }

    async fn status(&self, token: &str) -> Value {
        let response = self
            .client
            .post(self.url("/reader3/getAgentTaskStatus"))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap();
        let body: Value = response.json().await.unwrap();
        body["data"].clone()
    }

    /// 等待当前任务结束，返回最终快照。
    async fn wait_task_done(&self, token: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "task did not finish in time");
            let snapshot = self.status(token).await;
            // 快照在 runAgentTask 返回前就已建立（jobId 非空），running=false 即结束
            if snapshot["running"].as_bool() == Some(false)
                && snapshot["jobId"].as_str().is_some_and(|id| !id.is_empty())
            {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    async fn get_memory(&self, token: &str, book_id: &str) -> Value {
        let response = self
            .client
            .post(self.url("/reader3/getAiBookMemory"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&json!({"bookId": book_id}))
            .send()
            .await
            .unwrap();
        let body: Value = response.json().await.unwrap();
        body["data"].clone()
    }
}

#[tokio::test]
async fn agent_task_updates_to_current_with_tool_roundtrip() {
    if !node_available() {
        eprintln!("skip: Node 不可用，无法运行假 sidecar");
        return;
    }
    // 环境变量白名单哨兵：spawn 必须 env_clear + 白名单透传，
    // 子进程不能读到宿主的服务端凭据（假 sidecar 回报 envLeak:false）
    std::env::set_var("JWT_SECRET", "sentinel-jwt-secret");
    std::env::set_var("ADMIN_PASSWORD", "sentinel-admin-password");
    let server = TestServer::start(30).await;
    let token = server.login().await;

    let content = "\
第一章 开始
主角登场，正文若干。
第二章 搜索
正文里藏着暗号这个词。
第三章 结尾
最后章节的正文。";
    let (book_id, _) = server.upload_txt(&token, "流程测试.txt", content).await;

    let response = server
        .start_task(&token, &book_id, "update_to_current", Some(2))
        .await;
    assert_eq!(response.status().as_u16(), 200, "runAgentTask 失败");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["isSuccess"], json!(true));
    assert!(body["data"]["jobId"].as_str().is_some());

    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(snapshot["lastError"], Value::Null, "{snapshot}");
    assert_eq!(snapshot["targetChapterIndex"], json!(2));

    let memory = server.get_memory(&token, &book_id).await;
    assert_eq!(
        memory["processedChapterIndex"],
        json!(2),
        "应推进到最后一章"
    );
    let summary = memory["summary"].as_str().unwrap_or_default();
    // 工具回调拿到正文长度——章节内容真的从统一内容链路流到了 sidecar
    assert!(
        summary.contains("第0章正文"),
        "summary 应含正文长度回执：{summary}"
    );
    assert!(summary.contains("第2章正文"), "summary：{summary}");
    // 搜索钳制：模型要 0..99，服务端按当前已处理进度（第0章已处理）钳到 0
    assert!(
        summary.contains("搜索范围:0"),
        "搜索范围应被钳制：{summary}"
    );
    // 环境变量白名单：假 sidecar 不应见到 JWT_SECRET / ADMIN_PASSWORD
    assert!(
        summary.contains("envLeak:false"),
        "sidecar 子进程不应继承服务端凭据：{summary}"
    );
    // 章节窗口下发了邻近目录，快照也记录了目标章
    assert_eq!(snapshot["bookId"], json!(book_id));
}

#[tokio::test]
async fn agent_task_skips_non_story_chapters() {
    if !node_available() {
        eprintln!("skip: Node 不可用，无法运行假 sidecar");
        return;
    }
    let server = TestServer::start(30).await;
    let token = server.login().await;

    let content = "\
第一章 开始
正文一。
第二章 跳过
番外内容。
第三章 结尾
正文三。";
    let (book_id, _) = server.upload_txt(&token, "跳过测试.txt", content).await;

    let response = server
        .start_task(&token, &book_id, "update_to_current", Some(2))
        .await;
    assert_eq!(response.status().as_u16(), 200);

    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(snapshot["lastError"], Value::Null, "不应出错：{snapshot}");
    let memory = server.get_memory(&token, &book_id).await;
    assert_eq!(memory["processedChapterIndex"], json!(2));
    let summary = memory["summary"].as_str().unwrap_or_default();
    // 跳过章不产生正文回执，前后章照常处理
    assert!(summary.contains("第0章正文"), "summary：{summary}");
    assert!(summary.contains("第2章正文"), "summary：{summary}");
    assert!(
        !summary.contains("第1章正文"),
        "跳过章不应有正文回执：{summary}"
    );
}

#[tokio::test]
async fn agent_task_ignores_stale_result_with_mismatched_job_id() {
    if !node_available() {
        eprintln!("skip: Node 不可用，无法运行假 sidecar");
        return;
    }
    let server = TestServer::start(30).await;
    let token = server.login().await;

    // 「串帧」章：假 sidecar 在真实 result 之前先发一个 jobId 不匹配的
    // 迟到 result（无 memory 字段，等价于上一章 redraw 超时后迟到的帧）。
    // 修复前它会被当成当前章节的 result 收下，炸出「result 帧缺少 memory」；
    // 修复后按 jobId 跳过，本章仍以真实 result 成功。
    let content = "\
第一章 开始
正文一。
第二章 串帧
正文二。
第三章 结尾
正文三。";
    let (book_id, _) = server.upload_txt(&token, "串帧测试.txt", content).await;

    let response = server
        .start_task(&token, &book_id, "update_to_current", Some(2))
        .await;
    assert_eq!(response.status().as_u16(), 200);

    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(
        snapshot["lastError"],
        Value::Null,
        "迟到帧应被跳过：{snapshot}"
    );
    let memory = server.get_memory(&token, &book_id).await;
    assert_eq!(memory["processedChapterIndex"], json!(2));
    let summary = memory["summary"].as_str().unwrap_or_default();
    assert!(summary.contains("第1章正文"), "串帧章应正常处理：{summary}");
    assert!(summary.contains("第2章正文"), "后续章不受影响：{summary}");
}

#[tokio::test]
async fn agent_task_rejects_tool_call_with_mismatched_job_id() {
    if !node_available() {
        eprintln!("skip: Node 不可用，无法运行假 sidecar");
        return;
    }
    let server = TestServer::start(30).await;
    let token = server.login().await;

    // 「越权」章：假 sidecar 先发一个 jobId 不匹配的 tool_call（协议 v2 归属
    // 校验）。宿主应回一个带错误内容的 tool_result 并记 warn，而不是把内容
    // 应答给它或直接炸掉任务——本章随后仍以正常的取文回调完成。
    let content = "\
第一章 开始
正文一。
第二章 越权
正文二。
第三章 结尾
正文三。";
    let (book_id, _) = server.upload_txt(&token, "越权测试.txt", content).await;

    let response = server
        .start_task(&token, &book_id, "update_to_current", Some(2))
        .await;
    assert_eq!(response.status().as_u16(), 200);

    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(
        snapshot["lastError"],
        Value::Null,
        "jobId 不匹配的工具调用不应中断任务：{snapshot}"
    );
    let memory = server.get_memory(&token, &book_id).await;
    assert_eq!(memory["processedChapterIndex"], json!(2));
    let summary = memory["summary"].as_str().unwrap_or_default();
    // 越权调用收到带错误内容的 tool_result（ok:false），错误文案指明归属不匹配
    assert!(
        summary.contains("越权拒绝:工具调用被拒绝：jobId 与当前任务不匹配"),
        "越权调用应被拒绝：{summary}"
    );
    // 本章与后续章节照常走完正常工具回调
    assert!(summary.contains("第1章正文"), "越权章应正常处理：{summary}");
    assert!(summary.contains("第2章正文"), "后续章不受影响：{summary}");
}

#[tokio::test]
async fn agent_task_aborts_when_sidecar_emits_oversized_line() {
    if !node_available() {
        eprintln!("skip: Node 不可用，无法运行假 sidecar");
        return;
    }
    let server = TestServer::start(30).await;
    let token = server.login().await;

    // 「超长行」章：假 sidecar 吐一行超过 64MB 上限的“NDJSON”。宿主应报
    // 「单行超过上限」并中止任务，而不是无界把整行读进内存。
    let content = "\
第一章 超长行
正文一。";
    let (book_id, _) = server.upload_txt(&token, "超长行测试.txt", content).await;

    let response = server
        .start_task(&token, &book_id, "update_to_current", Some(0))
        .await;
    assert_eq!(response.status().as_u16(), 200);

    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(snapshot["running"], json!(false));
    let last_error = snapshot["lastError"].as_str().unwrap_or_default();
    assert!(
        last_error.contains("单行超过"),
        "应报单行超过上限：{snapshot}"
    );

    // 任务槽已释放，后续任务不被卡死。注意快照先于槽位清零翻转
    // （start_task 尾部：update_snapshot → kill child → 释放 active），
    // 杀掉还在写 64MB 的子进程需要时间，立即重试可能短暂 409——轮询等待。
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut slot_released = false;
    while Instant::now() < deadline {
        let retry = server
            .start_task(&token, &book_id, "update_to_current", Some(0))
            .await;
        match retry.status().as_u16() {
            200 => {
                slot_released = true;
                break;
            }
            409 => tokio::time::sleep(Duration::from_millis(100)).await,
            other => panic!("重试只应 200 或 409，收到 {other}"),
        }
    }
    assert!(slot_released, "任务槽应在合理时间内释放");
    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(snapshot["running"], json!(false));
}

#[tokio::test]
async fn agent_task_watchdog_kills_stuck_chapter() {
    if !node_available() {
        eprintln!("skip: Node 不可用，无法运行假 sidecar");
        return;
    }
    let server = TestServer::start(2).await; // 单章超时 2 秒
    let token = server.login().await;

    let content = "\
第一章 开始
正文一。
第二章 卡住
永不回应。";
    let (book_id, _) = server.upload_txt(&token, "卡住测试.txt", content).await;

    let response = server
        .start_task(&token, &book_id, "update_to_current", Some(2))
        .await;
    assert_eq!(response.status().as_u16(), 200);

    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(snapshot["running"], json!(false));
    let last_error = snapshot["lastError"].as_str().unwrap_or_default();
    assert!(last_error.contains("超时"), "看门狗应报超时：{snapshot}");
}

#[tokio::test]
async fn agent_task_sidecar_eof_releases_active_slot() {
    if !node_available() {
        eprintln!("skip: Node 不可用，无法运行假 sidecar");
        return;
    }
    let server = TestServer::start(30).await;
    let token = server.login().await;

    // 「闪退」章节让假 sidecar 直接 process.exit(1)：走 EOF 错误路径。
    // 这条路径曾经在 async 上下文里调 blocking_lock 直接 panic，
    // panic 跳过 spawn 尾部的清理，active 槽永久占用，后续任务全部 409。
    let content = "\
第一章 闪退
这章会让 sidecar 崩溃。";
    let (book_id, _) = server.upload_txt(&token, "闪退测试.txt", content).await;

    let response = server
        .start_task(&token, &book_id, "update_to_current", Some(0))
        .await;
    assert_eq!(response.status().as_u16(), 200);

    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(snapshot["running"], json!(false));
    let last_error = snapshot["lastError"].as_str().unwrap_or_default();
    assert!(
        last_error.contains("sidecar 意外退出"),
        "EOF 路径应报意外退出：{snapshot}"
    );
    // stderr 末尾应带进错误里（证明读到了 tail 而不是 panic）
    assert!(
        last_error.contains("FAKE_CRASH"),
        "错误应含 sidecar stderr 末尾：{snapshot}"
    );

    // 关键回归：active 槽已释放，再次提交不应该是 409
    let retry = server
        .start_task(&token, &book_id, "update_to_current", Some(0))
        .await;
    assert_eq!(
        retry.status().as_u16(),
        200,
        "任务槽应已释放，不应 409：{}",
        retry.status()
    );
    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(snapshot["running"], json!(false));
}

#[tokio::test]
async fn agent_task_cancel_kills_process_and_rejects_duplicate() {
    if !node_available() {
        eprintln!("skip: Node 不可用，无法运行假 sidecar");
        return;
    }
    let server = TestServer::start(30).await;
    let token = server.login().await;

    let content = "\
第一章 开始
正文一。
第二章 卡住
永不回应。";
    let (book_id, _) = server.upload_txt(&token, "取消测试.txt", content).await;

    let response = server
        .start_task(&token, &book_id, "update_to_current", Some(2))
        .await;
    assert_eq!(response.status().as_u16(), 200);

    // 等任务真正跑起来（快照 running=true）
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut started = false;
    while Instant::now() < deadline {
        let snapshot = server.status(&token).await;
        if snapshot["running"].as_bool() == Some(true) {
            started = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(started, "任务应已开始");

    // 任务进行中重复提交 → 409
    let duplicate = server
        .start_task(&token, &book_id, "update_to_current", None)
        .await;
    assert_eq!(duplicate.status().as_u16(), 409, "重复提交应 409");

    let cancelled = server
        .client
        .post(server.url("/reader3/cancelAgentTask"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancelled.status().as_u16(), 200);

    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(snapshot["running"], json!(false));
    assert_eq!(
        snapshot["lastError"],
        Value::Null,
        "取消不是错误：{snapshot}"
    );
    assert_eq!(snapshot["statusText"], json!("已取消"));
}

#[tokio::test]
async fn redraw_map_task_stores_image() {
    if !node_available() {
        eprintln!("skip: Node 不可用，无法运行假 sidecar");
        return;
    }
    let server = TestServer::start(30).await;
    let token = server.login().await;

    let content = "第一章 开始\n正文。";
    let book_id = server
        .upload_txt_as(&token, "地图测试.txt", content, "正常作者")
        .await;
    // 预置 map prompt，假 sidecar 会原样回显
    server
        .client
        .post(server.url("/reader3/saveAiBookMemory"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({
            "bookId": book_id,
            "enabled": true,
            "map": {"prompt": "绘制测试地图"},
            "mapDirty": true,
        }))
        .send()
        .await
        .unwrap();

    let response = server
        .start_task(&token, &book_id, "redraw_map", None)
        .await;
    assert_eq!(response.status().as_u16(), 200);

    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(snapshot["lastError"], Value::Null, "{snapshot}");
    let memory = server.get_memory(&token, &book_id).await;
    let image_url = memory["map"]["imageUrl"].as_str().expect("应有地图 URL");
    assert!(image_url.starts_with("/assets/"), "URL：{image_url}");
    assert!(image_url.contains("ai-maps"), "URL：{image_url}");
    assert_eq!(memory["mapDirty"], json!(false));
    assert_eq!(memory["map"]["prompt"], json!("绘制测试地图"));

    // 落盘文件真实存在
    let relative = image_url.trim_start_matches("/assets/");
    let file_path = server.temp_dir.join("assets").join(relative);
    assert!(
        file_path.exists(),
        "地图文件应落盘：{}",
        file_path.display()
    );
    assert_ne!(std::fs::read(&file_path).unwrap().len(), 0);
}

#[tokio::test]
async fn redraw_map_failure_falls_back_to_relationship_graph() {
    if !node_available() {
        eprintln!("skip: Node 不可用，无法运行假 sidecar");
        return;
    }
    let server = TestServer::start(30).await;
    let token = server.login().await;

    let content = "第一章 开始\n正文。";
    let book_id = server
        .upload_txt_as(&token, "地图失败测试.txt", content, "地图失败作者")
        .await;
    server
        .client
        .post(server.url("/reader3/saveAiBookMemory"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({
            "bookId": book_id,
            "enabled": true,
            "map": {"prompt": "绘制测试地图"},
            "mapDirty": true,
        }))
        .send()
        .await
        .unwrap();

    let response = server
        .start_task(&token, &book_id, "redraw_map", None)
        .await;
    assert_eq!(response.status().as_u16(), 200);

    let snapshot = server.wait_task_done(&token).await;
    assert_eq!(snapshot["running"], json!(false));
    let memory = server.get_memory(&token, &book_id).await;
    // 现状 applyMapFallbackToMemory 的字段结构
    assert_eq!(memory["map"]["fallback"], json!("relationship-graph"));
    assert!(memory["map"]["fallbackReason"]
        .as_str()
        .unwrap_or_default()
        .contains("已显示关系图"));
    assert_eq!(memory["mapDirty"], json!(true));
}
