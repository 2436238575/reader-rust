//! 集成测试共享的临时目录约定。
//!
//! Windows 下测试进程还活着时 SQLite 连接池锁着 reader.db，测试结束时的
//! `remove_dir_all` 会因文件被占用静默失败（`let _ =` 吞掉），每跑一轮
//! 就在 %TEMP% 留一堆孤儿目录。约定两条：
//! - 启动时清掉同名前缀的**往期**遗留（超过 10 分钟，属它的进程早退了）；
//! - 结尾删除用 `remove_dir_retried` 重试等句柄释放，仍失败就留给下一次启动。

use reader_rust::app::config::AppConfig;
use std::path::Path;

/// 集成测试通用的 AppConfig：临时目录、随机端口、固定账号、关闭登录限速。
/// 调用方按需覆盖字段（如 `allow_private_network`、`cookie_path`）。
#[allow(dead_code)]
pub fn base_config(temp_dir: &Path, jwt_secret: &str) -> AppConfig {
    AppConfig {
        server_host: "127.0.0.1".to_string(),
        server_port: 0,
        database_url: format!("sqlite:{}?mode=rwc", temp_dir.join("reader.db").display()),
        storage_dir: temp_dir.to_string_lossy().to_string(),
        web_root: temp_dir.to_string_lossy().to_string(),
        assets_dir: temp_dir.join("assets").to_string_lossy().to_string(),
        jwt_secret: jwt_secret.to_string(),
        request_timeout_secs: 10,
        admin_username: "reader1".to_string(),
        admin_password: "password123".to_string(),
        rate_limit_disabled: true,
        ..AppConfig::default()
    }
}

/// 在 127.0.0.1 的临时端口启动一个 axum Router，返回基础地址与任务句柄。
#[allow(dead_code)]
pub async fn spawn_router(app: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

/// 在 127.0.0.1 的临时端口启动主应用 Router（带 ConnectInfo，限速需要客户端地址）。
#[allow(dead_code)]
pub async fn spawn_app(app: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    (format!("http://{addr}"), handle)
}

/// 用固定测试账号登录，返回 accessToken。登录失败直接 panic，避免后续断言
/// 拿到 None 后报出无关的错误。
#[allow(dead_code)]
pub async fn login(client: &reqwest::Client, base_url: &str) -> String {
    login_as(client, base_url, "reader1", "password123").await
}

#[allow(dead_code)]
pub async fn login_as(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> String {
    let response = client
        .post(format!("{base_url}/reader3/login"))
        .json(&serde_json::json!({ "username": username, "password": password }))
        .send()
        .await
        .unwrap();
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        payload["isSuccess"],
        serde_json::json!(true),
        "登录应成功: {payload}"
    );
    payload["data"]["accessToken"].as_str().unwrap().to_string()
}

/// 保存一条书源，返回后端分配的 bookSourceId。
#[allow(dead_code)]
pub async fn save_source(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    source: &serde_json::Value,
) -> i64 {
    let response = client
        .post(format!("{base_url}/reader3/saveBookSource"))
        .bearer_auth(token)
        .json(source)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        payload["isSuccess"],
        serde_json::json!(true),
        "保存书源应成功: {payload}"
    );
    payload["data"]["bookSourceId"].as_i64().unwrap()
}

/// 清掉 %TEMP% 下往期测试的遗留目录（`reader-rust-` 前缀、超过 10 分钟）。
/// 只动老目录：正在跑的并行测试进程产生的目录是新的，不会被误删。
///
/// 共享模块：不同测试 crate 用到的函数不一样（有的只建目录不建服务），
/// 单个 crate 里未使用属正常，别让 clippy 的 dead_code 闸门误报。
#[allow(dead_code)]
pub fn clean_stale_temp_dirs() {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("reader-rust-")
        {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// 测试结尾的目录删除：重试几次等句柄释放；仍失败就留给下一次启动时清，
/// 不要再 `let _ =` 一次了事。
#[allow(dead_code)]
pub fn remove_dir_retried(path: impl AsRef<Path>) {
    for _ in 0..5 {
        if std::fs::remove_dir_all(path.as_ref()).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
