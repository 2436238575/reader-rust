use crate::crawler::url_guard;
use reqwest::{Client, Proxy};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 按用户命名空间隔离的 HTTP 客户端池。
///
/// 书源抓取会接触到大量站点下发的会话 Cookie。若所有用户共用同一个 reqwest
/// Client，Cookie jar 就是全局共享的：A 用户在某站点登录后，B 用户对该站点的
/// 请求会自动带上 A 的会话 Cookie，构成越权与账号串号。因此这里为每个
/// `user_ns` 维护一个独立 Client（各自独立的 cookie_store 与连接池）。
///
/// 客户端数量受 `USER_LIMIT` 约束（每个命名空间最多一个），不会无界增长。
#[derive(Clone)]
pub struct HttpClient {
    inner: Arc<Inner>,
}

struct Inner {
    timeout_secs: u64,
    proxy: Option<String>,
    clients: Mutex<HashMap<String, Client>>,
}

impl HttpClient {
    pub fn new(timeout_secs: u64, proxy: Option<String>) -> anyhow::Result<Self> {
        // 立刻构建一次，尽早暴露代理地址等配置错误
        build_guarded_client(timeout_secs, proxy.as_deref(), DEFAULT_USER_AGENT)?;
        Ok(Self {
            inner: Arc::new(Inner {
                timeout_secs,
                proxy,
                clients: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// 取某个用户命名空间下的独立客户端（独立 Cookie jar）。
    pub fn client_for(&self, user_ns: &str) -> anyhow::Result<Client> {
        if let Ok(map) = self.inner.clients.lock() {
            if let Some(client) = map.get(user_ns) {
                return Ok(client.clone());
            }
        }
        let client = build_guarded_client(
            self.inner.timeout_secs,
            self.inner.proxy.as_deref(),
            DEFAULT_USER_AGENT,
        )?;
        let mut map = self.inner.clients.lock().unwrap_or_else(|e| e.into_inner());
        Ok(map.entry(user_ns.to_string()).or_insert(client).clone())
    }
}

pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// 统一走出站守卫 builder：带超时 + 逐跳校验的重定向策略。
///
/// 保留 `cookie_store`：书源常依赖 Set-Cookie 维持会话，但该 jar 只属于
/// 调用方拿到的这个 Client，即只属于某一个 `user_ns`。
pub fn build_guarded_client(
    timeout_secs: u64,
    proxy: Option<&str>,
    user_agent: &str,
) -> anyhow::Result<Client> {
    let mut builder = url_guard::guarded_client_builder(
        Duration::from_secs(timeout_secs),
        user_agent,
    )
    .cookie_store(true);
    if let Some(p) = proxy {
        builder = builder.proxy(Proxy::all(p)?);
    }
    Ok(builder.build()?)
}
