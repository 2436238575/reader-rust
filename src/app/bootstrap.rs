use axum::Router;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

use crate::api::{self, AppState};
use crate::app::config::{self, AppConfig};
use crate::auth::{resolve_jwt_secret, AuthState};
use crate::crawler::http_client::HttpClient;
use crate::crawler::url_guard;
use crate::parser::rule_engine::RuleEngine;
use crate::service::{
    ai_book_service::AiBookService, ai_model_service::AiModelService,
    book_group_service::BookGroupService, book_service::BookService,
    book_source_service::BookSourceService, image_service::ImageService,
    json_document_service::JsonDocumentService, local_epub_book::LocalEpubBookService,
    local_txt_book::LocalTxtBookService, user_service::UserService,
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

    let state = build_state(cfg.clone()).await?;
    let app: Router = api::router::build_router(state);

    let addr = SocketAddr::new(cfg.server_host.parse()?, cfg.server_port);
    // 非环回监听 + 放行私网出站 = 把内网探测代理暴露给整个网络面，启动时明确提示
    if !addr.ip().is_loopback() && cfg.allow_private_network {
        tracing::warn!(
            "正在监听 {} 且 ALLOW_PRIVATE_NETWORK=true：公网/多用户部署建议显式设 \
             ALLOW_PRIVATE_NETWORK=false，否则出站抓取可直达内网地址",
            addr
        );
    }
    tracing::info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    tracing::info!("server stopped gracefully");
    Ok(())
}

/// 组装全部服务与共享状态。
///
/// 与 `run()` 拆开是为了让集成测试能直接起一套真实的路由，而不必复制一份
/// 装配逻辑——复制出来的那份迟早会和这里漂移。
pub async fn build_state(cfg: AppConfig) -> anyhow::Result<AppState> {
    // 出站守卫策略：默认放行私网——自托管单用户下局域网书源、本地模型服务
    // 是正常用法；多用户或公网暴露的部署应显式设 ALLOW_PRIVATE_NETWORK=false
    // 并配置 PRIVATE_NETWORK_WHITELIST（名单为空时仍是全部放行，见 url_guard 文档）。
    let allow_private = cfg.allow_private_network;
    url_guard::set_allow_private_network(allow_private);
    let whitelist = url_guard::parse_private_whitelist(&cfg.private_network_whitelist)
        .map_err(anyhow::Error::msg)?;
    url_guard::set_private_network_whitelist(whitelist);
    tracing::info!(
        "outbound policy: allow_private_network={allow_private}, whitelist_entries={}",
        url_guard::private_whitelist_len()
    );

    let storage_fs = StorageFs::new(&cfg.storage_dir, &cfg.assets_dir);
    storage_fs.ensure().await?;
    tracing::info!("storage ready: dir={}", cfg.storage_dir);

    let jwt_secret = resolve_jwt_secret(Some(&cfg.jwt_secret), &cfg.storage_dir).await?;

    let pool = db::init_pool(&cfg.database_url).await?;
    let repo = db::repo::BookSourceRepo::new(pool.clone());

    let http = HttpClient::new(cfg.request_timeout_secs, None)?;
    let parser = RuleEngine::new()?;
    let cache = FileCache::new(
        format!("{}/cache", cfg.storage_dir),
        cfg.cache_user_limit_bytes,
    );
    tracing::info!("core services initialized (db/http/rule engine/cache)");

    // 图片管道与书源抓取共用同一个 HttpClient（同一套出站守卫与代理配置）
    let image_service = Arc::new(ImageService::new(
        http.clone(),
        &cfg.storage_dir,
        cfg.cache_cover_limit_bytes,
    ));
    let book_service = Arc::new(
        BookService::new(http, parser, cache, &cfg.storage_dir)
            .with_user_book_limit(cfg.user_book_limit)
            .with_user_local_book_limit(cfg.user_local_book_limit)
            .with_review_cache(cfg.review_cache_ttl_secs, cfg.review_cache_user_limit_bytes),
    );
    let book_source_service = Arc::new(BookSourceService::new(repo));
    let local_txt_book_service = Arc::new(LocalTxtBookService::new(&cfg.storage_dir));
    let local_epub_book_service = Arc::new(LocalEpubBookService::new(&cfg.storage_dir));
    let json_document_service = Arc::new(JsonDocumentService::new(pool.clone(), &cfg.storage_dir));
    let user_service = Arc::new(UserService::new(
        cfg.clone(),
        pool.clone(),
        jwt_secret.clone(),
    ));
    // 唯一账号的建号与 ADMIN_PASSWORD 强制重置都在这里完成
    user_service.bootstrap_admin().await?;
    let book_group_service = Arc::new(BookGroupService::new(json_document_service.clone()));
    let ai_book_service = Arc::new(AiBookService::new(pool.clone(), &cfg.storage_dir));
    // 后端 AI 模型配置只来自环境变量（AI_TEXT_* / AI_IMAGE_* / AI_SPEECH_*），
    // 不落库、不下发；config::load() 已先加载 .env
    let ai_model_config = crate::model::ai_model::AiModelConfig::from_env();
    {
        // 启动时汇报各模型就绪状态：env 配了但没生效时（工作目录不对、
        // 二进制没更新）从这行就能看出来，不用等前端报「未配置」
        let ready = |enabled: bool, base_url: &str, model: &str| -> String {
            if enabled && !base_url.trim().is_empty() && !model.trim().is_empty() {
                format!("已启用（{}）", model)
            } else if enabled {
                "已启用但缺 BASE_URL/MODEL".to_string()
            } else {
                "未配置".to_string()
            }
        };
        tracing::info!(
            "AI 模型配置：文本={}，图片={}，语音={}",
            ready(
                ai_model_config.text.enabled,
                &ai_model_config.text.base_url,
                &ai_model_config.text.model
            ),
            ready(
                ai_model_config.image.enabled,
                &ai_model_config.image.base_url,
                &ai_model_config.image.model
            ),
            ready(
                ai_model_config.speech.enabled,
                &ai_model_config.speech.base_url,
                &ai_model_config.speech.model
            ),
        );
    }
    let ai_model_service = Arc::new(AiModelService::new(ai_model_config));

    Ok(AppState {
        config: cfg,
        image_service,
        auth: AuthState::new(pool.clone(), jwt_secret),
        book_service,
        book_source_service,
        user_service,
        book_group_service,
        local_txt_book_service,
        local_epub_book_service,
        json_document_service,
        ai_book_service,
        ai_model_service,
    })
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
