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
    agent_sidecar_service::AgentSidecarService, ai_book_service::AiBookService,
    ai_model_service::AiModelService, book_group_service::BookGroupService,
    book_service::BookService, book_source_service::BookSourceService, image_service::ImageService,
    json_document_service::JsonDocumentService, local_epub_book::LocalEpubBookService,
    local_txt_book::LocalTxtBookService, user_service::UserService,
};
use crate::storage::{cache::file_cache::FileCache, db, fs::storage_fs::StorageFs};

pub async fn run() -> anyhow::Result<()> {
    let cfg = config::load()?;

    // 初始化日志：控制台 + 文件双写（按天滚动到 <STORAGE_DIR>/logs/reader-rust.log.*，
    // 文件里不带 ANSI 颜色码；sidecar 的 stderr 经主进程 tracing 落同一个文件）。
    // _log_guard 保活非阻塞写线程，必须活到进程退出
    let logs_dir = std::path::Path::new(&cfg.storage_dir).join("logs");
    std::fs::create_dir_all(&logs_dir).ok();
    let (log_writer, _log_guard) = tracing_appender::non_blocking(
        tracing_appender::rolling::daily(&logs_dir, "reader-rust.log"),
    );
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(EnvFilter::new(cfg.log_level.clone()))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(log_writer)
                .with_ansi(false),
        )
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

    // watch 广播停机信号：graceful shutdown 与排空死线各持一个接收端
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let mut graceful_rx = shutdown_tx.subscribe();
    let mut deadline_rx = shutdown_tx.subscribe();
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = graceful_rx.wait_for(|signalled| *signalled).await;
    });
    // WithGracefulShutdown 是 IntoFuture 不是 Future，包一层 async 才能进 select!
    let mut serve = std::pin::pin!(async move { serve.await });

    // 优雅排空必须有硬上限：SSE 长连接（搜索/缓存/调试共 6 个端点）不会自然
    // 结束，等它们就是把进程挂到客户端断开或被 SIGKILL；挂住期间批量任务还在写。
    // 信号后最多留 10 秒给在途普通请求，超时直接退出。
    const GRACEFUL_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    let drain_deadline = async move {
        let _ = deadline_rx.wait_for(|signalled| *signalled).await;
        tokio::time::sleep(GRACEFUL_DRAIN_TIMEOUT).await;
    };

    tokio::select! {
        result = &mut serve => {
            result?;
            tracing::info!("server stopped gracefully");
        }        _ = drain_deadline => {
            tracing::warn!(
                "优雅关闭超时（{} 秒），强制退出（仍有长连接未结束）",
                GRACEFUL_DRAIN_TIMEOUT.as_secs()
            );
        }
    }
    Ok(())
}

/// 组装全部服务与共享状态。
///
/// 与 `run()` 拆开是为了让集成测试能直接起一套真实的路由，而不必复制一份
/// 装配逻辑——复制出来的那份迟早会和这里漂移。
pub async fn build_state(cfg: AppConfig) -> anyhow::Result<AppState> {
    // 出站守卫策略：默认拦截私网（安全优先）。自托管单用户、书源/模型服务在
    // 局域网的场景可显式设 ALLOW_PRIVATE_NETWORK=true 整体放行，或用
    // PRIVATE_NETWORK_WHITELIST 精确放行（名单为空时一律拦截，见 url_guard 文档）。
    let allow_private = cfg.allow_private_network;
    url_guard::set_allow_private_network(allow_private);
    let whitelist = url_guard::parse_private_whitelist(&cfg.private_network_whitelist)
        .map_err(anyhow::Error::msg)?;
    url_guard::set_private_network_whitelist(whitelist);
    tracing::info!(
        "outbound policy: allow_private_network={allow_private}, whitelist_entries={}",
        url_guard::private_whitelist_len()
    );
    if !allow_private && url_guard::private_whitelist_len() == 0 {
        tracing::info!(
            "私网出站全部拦截（默认安全策略）；书源/模型服务在局域网时，请配置 \
             PRIVATE_NETWORK_WHITELIST 精确放行，或设 ALLOW_PRIVATE_NETWORK=true 整体放行"
        );
    }

    let storage_fs = StorageFs::new(&cfg.storage_dir, &cfg.assets_dir);
    storage_fs.ensure().await?;
    tracing::info!("storage ready: dir={}", cfg.storage_dir);

    let jwt_secret = resolve_jwt_secret(Some(&cfg.jwt_secret), &cfg.storage_dir).await?;

    let pool = db::init_pool(&cfg.database_url).await?;
    let repo = db::repo::BookSourceRepo::new(pool.clone());
    let book_repo = db::repo::BookRepo::new(pool.clone());
    let shelf_repo = db::repo::ShelfBookRepo::new(pool.clone());

    let http = HttpClient::new(cfg.request_timeout_secs, None)?;
    // 书源 JS 的 java.ajax 等内部请求与主抓取共用同一超时口径
    crate::parser::js::set_js_http_timeout(cfg.request_timeout_secs);
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
        BookService::new(http, parser, cache, &cfg.storage_dir, book_repo, shelf_repo)
            .with_user_book_limit(cfg.user_book_limit)
            .with_user_local_book_limit(cfg.user_local_book_limit)
            .with_review_cache(cfg.review_cache_ttl_secs, cfg.review_cache_user_limit_bytes),
    );
    let book_source_service = Arc::new(BookSourceService::new(repo));
    let local_txt_book_service = Arc::new(LocalTxtBookService::new(&cfg.storage_dir));
    let local_epub_book_service = Arc::new(LocalEpubBookService::new(&cfg.storage_dir));
    let json_document_service = Arc::new(JsonDocumentService::new(pool.clone()));
    let user_service = Arc::new(UserService::new(
        cfg.clone(),
        pool.clone(),
        jwt_secret.clone(),
    ));
    // 唯一账号的建号与 ADMIN_PASSWORD 强制重置都在这里完成
    user_service.bootstrap_admin().await?;
    let book_group_service = Arc::new(BookGroupService::new(
        crate::storage::db::repo::BookGroupRepo::new(pool.clone()),
    ));
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
    let agent_sidecar_service = Arc::new(AgentSidecarService::new(
        book_service.clone(),
        book_source_service.clone(),
        local_txt_book_service.clone(),
        local_epub_book_service.clone(),
        ai_book_service.clone(),
        ai_model_service.clone(),
        &cfg.agent_sidecar_command,
        &cfg.agent_sidecar_uv,
        cfg.agent_sidecar_enabled,
        cfg.agent_sidecar_chapter_timeout_secs,
        &cfg.assets_dir,
    ));
    let probe_service = agent_sidecar_service.clone();
    tokio::spawn(async move {
        probe_service.probe().await;
    });

    let chapter_edit_service = Arc::new(
        crate::service::chapter_edit_service::ChapterEditService::new(
            &cfg.storage_dir,
            crate::storage::db::repo::ChapterEditRepo::new(pool.clone()),
        ),
    );

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
        chapter_edit_service,
        git_diff_service: Arc::new(crate::service::git_diff_service::GitDiffService::new()),
        json_document_service,
        bookmark_repo: db::repo::BookmarkRepo::new(pool.clone()),
        replace_rule_repo: db::repo::ReplaceRuleRepo::new(pool.clone()),
        ai_book_service,
        ai_model_service,
        agent_sidecar_service,
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
