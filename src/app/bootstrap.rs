use axum::Router;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

use crate::api::{self, AppState};
use crate::app::config;
use crate::crawler::http_client::HttpClient;
use crate::crawler::url_guard;
use crate::parser::rule_engine::RuleEngine;
use crate::service::{
    ai_book_service::AiBookService, ai_model_service::AiModelService,
    book_group_service::BookGroupService, book_service::BookService,
    book_source_service::BookSourceService, json_document_service::JsonDocumentService,
    local_epub_book::LocalEpubBookService, local_txt_book::LocalTxtBookService,
    update_service::UpdateService, user_service::UserService,
};
use crate::storage::{cache::file_cache::FileCache, db, fs::storage_fs::StorageFs};

pub async fn run() -> anyhow::Result<()> {
    let cfg = config::load()?;

    // 初始化日志：之后所有输出统一走 tracing，不再直接 println!/eprintln!
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(cfg.log_level.clone()))
        .init();

    // 把 panic 也记录进 tracing，便于在日志里定位崩溃位置；
    // 同时保留默认行为（stderr + abort 策略交给运行时）
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "进程内发生 panic");
        default_panic_hook(info);
    }));

    // 出站守卫策略：默认跟随 SECURE（本地单用户放行私网，多用户/公网拦截）
    let allow_private = cfg
        .allow_private_network
        .unwrap_or_else(|| url_guard::default_allow_private_network(cfg.secure));
    url_guard::set_allow_private_network(allow_private);
    tracing::info!(
        "outbound policy: allow_private_network={} (secure={})",
        allow_private,
        cfg.secure
    );

    let storage_fs = StorageFs::new(&cfg.storage_dir, &cfg.assets_dir);
    storage_fs.ensure().await?;
    tracing::info!("storage ready: dir={}", cfg.storage_dir);

    let pool = db::init_pool(&cfg.database_url).await?;
    let repo = db::repo::BookSourceRepo::new(pool.clone());

    let http = HttpClient::new(cfg.request_timeout_secs, None)?;
    let parser = RuleEngine::new()?;
    let cache = FileCache::new(format!("{}/cache", cfg.storage_dir));
    tracing::info!("core services initialized (db/http/rule engine/cache)");

    let book_service = Arc::new(BookService::new(http, parser, cache, &cfg.storage_dir));
    let book_source_service = Arc::new(BookSourceService::new(repo, &cfg.storage_dir));
    let local_txt_book_service = Arc::new(LocalTxtBookService::new(&cfg.storage_dir));
    let local_epub_book_service = Arc::new(LocalEpubBookService::new(&cfg.storage_dir));
    let json_document_service = Arc::new(JsonDocumentService::new(pool.clone(), &cfg.storage_dir));
    let user_service = Arc::new(UserService::new(cfg.clone(), pool.clone()));
    user_service.migrate_legacy_users_from_json().await?;
    let book_group_service = Arc::new(BookGroupService::new(json_document_service.clone()));
    let ai_book_service = Arc::new(AiBookService::new(pool.clone(), &cfg.storage_dir));
    let ai_model_service = Arc::new(AiModelService::new(
        json_document_service.clone(),
        &cfg.storage_dir,
    ));
    let update_service = Arc::new(UpdateService::new(
        json_document_service.clone(),
        cfg.request_timeout_secs,
        format!("v{}", env!("CARGO_PKG_VERSION")),
    )?);

    let state = AppState {
        config: cfg.clone(),
        book_service,
        book_source_service,
        user_service,
        book_group_service,
        local_txt_book_service,
        local_epub_book_service,
        json_document_service,
        ai_book_service,
        ai_model_service,
        update_service,
    };

    let app: Router = api::router::build_router(state);

    let addr = SocketAddr::new(cfg.server_host.parse()?, cfg.server_port);
    tracing::info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("server stopped gracefully");
    Ok(())
}

/// 等待优雅停机信号：SIGINT（Ctrl+C）或 SIGTERM（容器编排下发）。
///
/// 收到信号后停止接收新连接，等已有请求处理完再退出，
/// 避免部署/重启时粗暴掐断正在进行的请求。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received, draining in-flight requests");
}
