use crate::crawler::url_guard;
use reqwest::{Client, Proxy};
use std::time::Duration;

#[derive(Clone)]
pub struct HttpClient {
    client: Client,
}

impl HttpClient {
    pub fn new(timeout_secs: u64, proxy: Option<String>) -> anyhow::Result<Self> {
        let user_agent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
        // 统一走守卫 builder：带超时 + 逐跳校验的重定向策略
        let mut builder = url_guard::guarded_client_builder(
            Duration::from_secs(timeout_secs),
            user_agent,
        )
        .cookie_store(true);
        if let Some(p) = proxy {
            builder = builder.proxy(Proxy::all(p)?);
        }
        let client = builder.build()?;
        Ok(Self { client })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }
}
