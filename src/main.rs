mod api;
mod app;
mod crawler;
mod error;
mod model;
mod parser;
mod service;
mod storage;
mod util;

use app::bootstrap::run;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run().await.inspect_err(|e| {
        // tracing 可能在 config 加载失败时尚未初始化，这里兜底打到 stderr
        eprintln!("server exited with error: {e:?}");
    })
}
