use crate::api::{handlers, AppState};
use crate::auth::{optional_auth, require_auth};
use axum::{
    extract::DefaultBodyLimit,
    http::HeaderValue,
    middleware,
    routing::{any, get, post},
    Router,
};
use std::path::PathBuf;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

/// 构建 CORS 层。
///
/// 此前使用 `very_permissive()`，等于向任意站点反射 Origin 并允许携带凭据，
/// 任何第三方页面都能用受害者的登录态读取本服务响应。默认改为**仅同源**
/// （不发送 CORS 头）；确需跨域时用 `CORS_ALLOWED_ORIGINS` 显式列出来源。
fn build_cors_layer(cors_allowed_origins: &str) -> CorsLayer {
    let origins: Vec<HeaderValue> = cors_allowed_origins
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .filter_map(|origin| origin.parse::<HeaderValue>().ok())
        .collect();
    if origins.is_empty() {
        return CorsLayer::new();
    }
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(Any)
        .allow_headers(Any)
}

pub fn build_router(state: AppState) -> Router {
    // ── 公开：无需任何凭据 ──
    let public = Router::new()
        .route("/health", get(handlers::health))
        .route("/reader3/login", post(handlers::login))
        .with_state(state.clone());

    // ── 可选鉴权：未登录也返回成功，只是内容为空 ──
    let optional = Router::new()
        .route("/reader3/getUserInfo", get(handlers::get_user_info))
        .route("/reader3/logout", post(handlers::logout))
        .layer(middleware::from_fn_with_state(state.clone(), optional_auth))
        .with_state(state.clone());

    // ── WebDAV 文件接口：自带 HTTP Basic 认证，不经 JWT ──
    let webdav = Router::new()
        .route("/reader3/webdav/*path", any(handlers::webdav_handler))
        .with_state(state.clone());

    // ── 其余全部需要登录 ──
    let protected = Router::new()
        .route(
            "/reader3/getBookSource",
            get(handlers::get_book_source).post(handlers::get_book_source),
        )
        .route(
            "/reader3/getBookSources",
            get(handlers::get_book_sources).post(handlers::get_book_sources),
        )
        .route(
            "/reader3/loginBookSource",
            post(handlers::login_book_source),
        )
        .route(
            "/reader3/getExploreKinds",
            post(handlers::get_explore_kinds),
        )
        .route(
            "/reader3/testBookSources",
            post(handlers::test_book_sources),
        )
        .route(
            "/reader3/deleteInvalidBookSources",
            post(handlers::delete_invalid_book_sources),
        )
        .route("/reader3/bookSourceProxy", any(handlers::book_source_proxy))
        .route(
            "/reader3/bookSourceClientLog",
            any(handlers::book_source_client_log),
        )
        .route("/reader3/saveBookSource", post(handlers::save_book_source))
        .route(
            "/reader3/saveBookSources",
            post(handlers::save_book_sources),
        )
        .route(
            "/reader3/deleteBookSource",
            post(handlers::delete_book_source),
        )
        .route(
            "/reader3/deleteBookSources",
            post(handlers::delete_book_sources),
        )
        .route(
            "/reader3/deleteAllBookSources",
            post(handlers::delete_all_book_sources),
        )
        .route(
            "/reader3/readRemoteSourceFile",
            post(handlers::read_remote_source_file),
        )
        .route("/reader3/readSourceFile", post(handlers::read_source_file))
        .route(
            "/reader3/searchBook",
            get(handlers::search_book).post(handlers::search_book),
        )
        .route(
            "/reader3/exploreBook",
            get(handlers::explore_book).post(handlers::explore_book),
        )
        .route(
            "/reader3/searchBookMulti",
            get(handlers::search_book_multi).post(handlers::search_book_multi),
        )
        .route("/reader3/getBookshelf", get(handlers::get_bookshelf))
        .route(
            "/reader3/getShelfBook",
            get(handlers::get_shelf_book).post(handlers::get_shelf_book),
        )
        .route(
            "/reader3/getShelfBookWithCacheInfo",
            get(handlers::get_shelf_book_with_cache_info),
        )
        .route(
            "/reader3/getBookGroups",
            get(handlers::get_book_groups).post(handlers::get_book_groups),
        )
        .route("/reader3/saveBookGroup", post(handlers::save_book_group))
        .route(
            "/reader3/saveBookGroupOrder",
            post(handlers::save_book_group_order),
        )
        .route(
            "/reader3/deleteBookGroup",
            post(handlers::delete_book_group),
        )
        .route(
            "/reader3/saveBookGroupId",
            post(handlers::save_book_group_id),
        )
        .route(
            "/reader3/addBookGroupMulti",
            post(handlers::add_book_group_multi),
        )
        .route(
            "/reader3/removeBookGroupMulti",
            post(handlers::remove_book_group_multi),
        )
        .route("/reader3/uploadTxtBook", post(handlers::upload_txt_book))
        .route("/reader3/uploadEpubBook", post(handlers::upload_epub_book))
        .route("/reader3/saveBook", post(handlers::save_book))
        .route("/reader3/saveBooks", post(handlers::save_books))
        .route("/reader3/setBookSource", post(handlers::set_book_source))
        .route("/reader3/deleteBook", post(handlers::delete_book))
        .route("/reader3/deleteBooks", post(handlers::delete_books))
        .route(
            "/reader3/saveBookProgress",
            post(handlers::save_book_progress),
        )
        .route(
            "/reader3/getBookInfo",
            get(handlers::get_book_info).post(handlers::get_book_info),
        )
        .route(
            "/reader3/getChapterList",
            get(handlers::get_chapter_list).post(handlers::get_chapter_list),
        )
        .route(
            "/reader3/getBookContent",
            get(handlers::get_book_content).post(handlers::get_book_content),
        )
        .route(
            "/reader3/getChapterImages",
            get(handlers::get_chapter_images).post(handlers::get_chapter_images),
        )
        .route(
            "/reader3/getChapterComments",
            get(handlers::get_chapter_comments).post(handlers::get_chapter_comments),
        )
        .route(
            "/reader3/getParaCommentIndex",
            get(handlers::get_para_comment_index).post(handlers::get_para_comment_index),
        )
        .route(
            "/reader3/getParaComments",
            get(handlers::get_para_comments).post(handlers::get_para_comments),
        )
        .route(
            "/reader3/deleteBookCache",
            post(handlers::delete_book_cache),
        )
        .route("/reader3/purgeCache", post(handlers::purge_cache))
        .route("/reader3/cacheStats", get(handlers::cache_stats))
        .route(
            "/reader3/getInvalidBookSources",
            post(handlers::get_invalid_book_sources),
        )
        .route(
            "/reader3/cacheBookSSE",
            get(handlers::cache_book_sse).post(handlers::cache_book_sse),
        )
        .route(
            "/reader3/searchBookMultiSSE",
            get(handlers::search_book_multi_sse),
        )
        .route(
            "/reader3/searchBookSourceSSE",
            get(handlers::search_book_source_sse),
        )
        .route(
            "/reader3/getAvailableBookSource",
            get(handlers::get_available_book_source).post(handlers::get_available_book_source),
        )
        .route(
            "/reader3/getAvailableBookSourceSSE",
            get(handlers::get_available_book_source_sse),
        )
        .route(
            "/reader3/bookSourceDebugSSE",
            get(handlers::book_source_debug_sse),
        )
        .route("/reader3/cover", get(handlers::get_book_cover))
        .route(
            "/reader3/localEpubAsset",
            get(handlers::get_local_epub_asset),
        )
        .route("/reader3/getBookmarks", get(handlers::get_bookmarks))
        .route("/reader3/saveBookmark", post(handlers::save_bookmark))
        .route("/reader3/saveBookmarks", post(handlers::save_bookmarks))
        .route("/reader3/deleteBookmark", post(handlers::delete_bookmark))
        .route("/reader3/deleteBookmarks", post(handlers::delete_bookmarks))
        .route(
            "/reader3/getAiBookMemory",
            get(handlers::get_ai_book_memory).post(handlers::get_ai_book_memory),
        )
        .route(
            "/reader3/saveAiBookMemory",
            post(handlers::save_ai_book_memory),
        )
        .route(
            "/reader3/deleteAiBookMemory",
            post(handlers::delete_ai_book_memory),
        )
        .route(
            "/reader3/getAiModelConfig",
            get(handlers::get_ai_model_config),
        )
        .route("/reader3/aiProxy", post(handlers::ai_proxy))
        .route("/reader3/aiProxyImage", post(handlers::ai_proxy_image))
        .route("/reader3/getReplaceRules", get(handlers::get_replace_rules))
        .route(
            "/reader3/saveReplaceRule",
            post(handlers::save_replace_rule),
        )
        .route(
            "/reader3/saveReplaceRules",
            post(handlers::save_replace_rules),
        )
        .route(
            "/reader3/deleteReplaceRule",
            post(handlers::delete_replace_rule),
        )
        .route(
            "/reader3/deleteReplaceRules",
            post(handlers::delete_replace_rules),
        )
        .route(
            "/reader3/getWebdavFileList",
            get(handlers::get_webdav_file_list),
        )
        .route("/reader3/getWebdavFile", get(handlers::get_webdav_file))
        .route(
            "/reader3/uploadFileToWebdav",
            post(handlers::upload_file_to_webdav),
        )
        .route(
            "/reader3/deleteWebdavFile",
            post(handlers::delete_webdav_file),
        )
        .route(
            "/reader3/deleteWebdavFileList",
            post(handlers::delete_webdav_file_list),
        )
        .route("/reader3/changePassword", post(handlers::change_password))
        .route("/reader3/saveUserConfig", post(handlers::save_user_config))
        .route("/reader3/getUserConfig", get(handlers::get_user_config))
        .route("/reader3/uploadFile", post(handlers::upload_file))
        .route("/reader3/deleteFile", post(handlers::delete_file))
        .route("/reader3/getTxtTocRules", get(handlers::get_txt_toc_rules))
        .route(
            "/reader3/getVersionUpdate",
            get(handlers::get_version_update),
        )
        .route(
            "/reader3/dismissVersionUpdate",
            post(handlers::dismiss_version_update),
        )
        .route(
            "/reader3/saveAiModelConfig",
            post(handlers::save_ai_model_config),
        )
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .with_state(state.clone());

    // `/reader3` 是纯 API 命名空间：未匹配的路径返回 JSON 404，
    // 而不是落到静态文件服务上得到一个空响应体的 404。
    // 静态路由优先级高于通配路由，因此这里的通配只兜住真正未注册的路径。
    let api = Router::new()
        .merge(public)
        .merge(optional)
        .merge(webdav)
        .merge(protected)
        .route("/reader3", any(handlers::api_not_found))
        .route("/reader3/", any(handlers::api_not_found))
        .route("/reader3/*rest", any(handlers::api_not_found));

    let web_root = state.config.web_root.clone();
    let assets_root = state.config.assets_dir.clone();
    let web_assets_root = PathBuf::from(&web_root).join("assets");

    // 静态资源。
    //
    // 前端使用 hash 路由（`createWebHashHistory`），深链接不依赖服务端回落，
    // 因此这里**不提供 SPA fallback**：只有真实存在的文件才会被返回，
    // 其余路径一律 404。`/` 显式指向 index.html；dist 根目录下的
    // sw.js / site.webmanifest / favicon 等 PWA 资源仍按文件名直接可取。
    // 缓存策略：assets 带 hash 文件名 → 一年 immutable；index.html / sw.js
    // 等无 hash 文件 → no-cache（每次重验证拿 304）。此前完全不发
    // Cache-Control，浏览器每次回源拉全部资源。
    let immutable_cache = SetResponseHeaderLayer::overriding(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    let no_cache = SetResponseHeaderLayer::if_not_present(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );

    let assets_web = Router::new()
        .nest_service(
            "/assets",
            ServeDir::new(web_assets_root).not_found_service(ServeDir::new(assets_root)),
        )
        .layer(immutable_cache);

    let static_web = Router::new()
        .route_service(
            "/",
            ServeFile::new(PathBuf::from(&web_root).join("index.html")),
        )
        .merge(assets_web)
        .fallback_service(ServeDir::new(web_root).append_index_html_on_directories(false))
        .layer(no_cache)
        // 压缩只加在静态层：/reader3 的 SSE 流式响应不能被压缩层缓冲。
        // tower-http 0.5 由 compression-gzip feature 直接启用 gzip，无需再逐算法开启
        .layer(CompressionLayer::new());

    Router::new()
        .merge(api)
        .merge(static_web)
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        // 自定义 span：只记录 method + path，不记 query string。
        // accessToken 允许经查询参数传递（SSE 无法设置请求头），
        // 默认 span 记录完整 URI 等于把凭据写进访问日志。
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    path = %request.uri().path(),
                )
            }),
        )
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(build_cors_layer(&state.config.cors_allowed_origins))
}
