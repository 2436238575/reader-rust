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

/// 公开/可选鉴权组的请求体上限（1MB）：登录与令牌刷新只收小 JSON，
/// 未登录就能触达的端点不值得按全局兜底值缓冲 100MB。
const PUBLIC_BODY_LIMIT_BYTES: usize = 1024 * 1024;
/// 登录后接口的默认请求体上限（8MB）。
const PROTECTED_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;
/// 批量导入书源（`/reader3/saveBookSources`）的请求体上限。
const SAVE_SOURCES_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
/// 大上传路由与最外层兜底共用的请求体上限。
const MAX_BODY_LIMIT_BYTES: usize = 100 * 1024 * 1024;

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
        // Refresh Token 本身就是凭据，不能挂在 require_auth 后面
        .route("/reader3/refreshToken", post(handlers::refresh_token))
        // 请求体上限按下组下发：DefaultBodyLimit 经请求扩展覆盖，最内层先生效
        // （axum-core 0.4 的 DefaultBodyLimitService 对扩展是 insert 覆盖写），
        // 因此这里的 1MB 优先于最外层的 100MB 兜底
        .layer(DefaultBodyLimit::max(PUBLIC_BODY_LIMIT_BYTES))
        .with_state(state.clone());

    // ── 可选鉴权：未登录也返回成功，只是内容为空 ──
    let optional = Router::new()
        .route("/reader3/getUserInfo", get(handlers::get_user_info))
        .route("/reader3/logout", post(handlers::logout))
        .layer(middleware::from_fn_with_state(state.clone(), optional_auth))
        .layer(DefaultBodyLimit::max(PUBLIC_BODY_LIMIT_BYTES))
        .with_state(state.clone());

    // ── WebDAV 文件接口：自带 HTTP Basic 认证，不经 JWT ──
    let webdav = Router::new()
        // 裸根与尾斜杠根都要显式注册：matchit 0.7 的 *path 通配不匹配空段，
        // 缺了会落到静态文件兜底上（PROPFIND 变成 405）
        .route("/reader3/webdav", any(handlers::webdav_handler))
        .route("/reader3/webdav/", any(handlers::webdav_handler))
        .route("/reader3/webdav/*path", any(handlers::webdav_handler))
        .with_state(state.clone());

    // ── 大请求体路由：先挂各自放宽的上限（最内层先生效），再并入 protected ──
    // 整本书/大文件上传（TXT、EPUB、章节正文导入、资源与 WebDAV 中转）
    let big_body_routes = Router::new()
        .route("/reader3/readSourceFile", post(handlers::read_source_file))
        .route("/reader3/uploadTxtBook", post(handlers::upload_txt_book))
        .route("/reader3/uploadEpubBook", post(handlers::upload_epub_book))
        .route(
            "/reader3/importChapterText",
            post(handlers::import_chapter_text),
        )
        // 预览与导入共用 read_chapter_import_request（TXT 上限 50MB），
        // 预览留在 8MB 组会让 8-50MB 的 TXT 在第一步就 413
        .route(
            "/reader3/previewChapterImport",
            post(handlers::preview_chapter_import),
        )
        // JSON body 携带整章正文，与 TXT 导入同属大文本入口
        .route(
            "/reader3/importChapterVersion",
            post(handlers::import_chapter_version),
        )
        .route("/reader3/uploadFile", post(handlers::upload_file))
        .route(
            "/reader3/uploadFileToWebdav",
            post(handlers::upload_file_to_webdav),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_LIMIT_BYTES));
    // 批量导入书源：一次性贴几 MB 到几十 MB 的 JSON 数组
    let bulk_sources_routes = Router::new()
        .route(
            "/reader3/saveBookSources",
            post(handlers::save_book_sources),
        )
        .layer(DefaultBodyLimit::max(SAVE_SOURCES_BODY_LIMIT_BYTES));

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
            "/reader3/saveChapterEdit",
            post(handlers::save_chapter_edit),
        )
        .route(
            "/reader3/getChapterEdit",
            get(handlers::get_chapter_edit).post(handlers::get_chapter_edit),
        )
        .route(
            "/reader3/listChapterEdits",
            get(handlers::list_chapter_edits).post(handlers::list_chapter_edits),
        )
        .route(
            "/reader3/deleteChapterEdit",
            post(handlers::delete_chapter_edit),
        )
        .route(
            "/reader3/getChapterTimeline",
            get(handlers::get_chapter_timeline).post(handlers::get_chapter_timeline),
        )
        .route(
            "/reader3/getChapterVersion",
            get(handlers::get_chapter_version).post(handlers::get_chapter_version),
        )
        .route(
            "/reader3/getChapterVersionDiff",
            get(handlers::get_chapter_version_diff).post(handlers::get_chapter_version_diff),
        )
        .route(
            "/reader3/selectChapterVersion",
            post(handlers::select_chapter_version),
        )
        .route(
            "/reader3/resetBookToUpstream",
            post(handlers::reset_book_to_upstream),
        )
        .route(
            "/reader3/getChapterImportDiff",
            post(handlers::get_chapter_import_diff),
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
            "/reader3/searchCachedContent",
            get(handlers::search_cached_content).post(handlers::search_cached_content),
        )
        .route(
            "/reader3/getInvalidBookSources",
            post(handlers::get_invalid_book_sources),
        )
        .route("/reader3/cacheBookSSE", get(handlers::cache_book_sse))
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
        // 图片管道：前端只拿 id，不接触书源地址（见 service/image_service.rs）
        .route("/reader3/image/:id", get(handlers::get_image))
        .route(
            "/reader3/localEpubAsset",
            get(handlers::get_local_epub_asset).layer(middleware::from_fn(sandbox_epub_svg_assets)),
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
        .route("/reader3/runAgentTask", post(handlers::run_agent_task))
        .route(
            "/reader3/getAgentTaskStatus",
            post(handlers::get_agent_task_status),
        )
        .route(
            "/reader3/cancelAgentTask",
            post(handlers::cancel_agent_task),
        )
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
        .route("/reader3/getUserdata", get(handlers::get_userdata))
        .route("/reader3/saveUserdata", post(handlers::save_userdata))
        .route("/reader3/deleteFile", post(handlers::delete_file))
        // 组级默认上限（8MB）：挂在普通路由之后、合并放宽子路由之前，
        // 只包住本组常规接口；放宽路由在各自子路由上已带更内侧的上限
        .layer(DefaultBodyLimit::max(PROTECTED_BODY_LIMIT_BYTES))
        .merge(bulk_sources_routes)
        .merge(big_body_routes)
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
    // 缓存策略：assets 带 hash 文件名 → 一年 immutable；fonts 下的字体分片
    // 文件名同样带内容 hash → 一年 immutable；index.html / sw.js
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
            // 用 fallback 而不是 not_found_service：后者会把回落响应的状态码
            // 强制改成 404（tower-http 0.5 的 SetStatus 包装），上传文件此前
            // 一直带着 404 状态直出，缓存与 fetch 都会当成不存在
            ServeDir::new(web_assets_root).fallback(ServeDir::new(assets_root)),
        )
        // /assets 同源直出上传文件：nosniff + sandbox CSP。对图片/字体等
        // 子资源加载无影响；直接打开 HTML/SVG 时脚本不执行、表单不提交
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox"),
        ))
        .layer(immutable_cache.clone());

    // 自托管字体（MiSans 分片）：文件名带内容 hash，与 assets 同档缓存
    let fonts_web = Router::new()
        .nest_service(
            "/fonts",
            ServeDir::new(PathBuf::from(&web_root).join("fonts")),
        )
        .layer(immutable_cache);

    // 首页：CSP 只加在 HTML 响应上（脚本/样式/连接全部自托管收口到
    // 'self'，样式保留 unsafe-inline 供 Vue 运行时注入），不扩散到
    // /assets 的 JS/CSS——那里的 sandbox CSP 语义不同
    let index_web = Router::new()
        .route_service(
            "/",
            ServeFile::new(PathBuf::from(&web_root).join("index.html")),
        )
        .layer(middleware::from_fn(apply_html_csp));

    let static_web = Router::new()
        .merge(index_web)
        .merge(assets_web)
        .merge(fonts_web)
        .fallback_service(ServeDir::new(web_root).append_index_html_on_directories(false))
        .layer(no_cache);

    Router::new()
        .merge(api)
        .merge(static_web)
        // gzip 压缩挂在最外层：/reader3 的 JSON 与静态资源共用一层（不重复挂）。
        // tower-http 的 DefaultPredicate 已排除 gRPC、image/*、text/event-stream
        // 与小于 32 字节的响应——SSE 事件流走 Identity 透传，不会被压缩层缓冲，
        // 图片管道与字体分片也不会被二次压缩
        .layer(CompressionLayer::new())
        // 最外层兜底：只对没挂组级/路由级上限的路径生效（如 WebDAV）。
        // DefaultBodyLimit 走请求扩展覆盖，最内层的限制先生效
        .layer(DefaultBodyLimit::max(MAX_BODY_LIMIT_BYTES))
        // SSE 响应统一加 X-Accel-Buffering: no：nginx 默认 proxy_buffering on，
        // 会把整条事件流攒到结束才一次性转发——直连后端进度逐帧正常、经反代
        // 就变成「缓存结束后进度一下走完」。这个头是 nginx 关流式缓冲的标准开关。
        .layer(middleware::from_fn(disallow_proxy_buffering))
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
        // 全局安全响应头：挂在最外层对所有响应统一补齐，if_not_present
        // 不覆盖各路由已特化的值。X-Frame-Options 用 SAMEORIGIN 而不是
        // DENY：SourcesView 用同源 iframe 打开 bookSourceProxy，DENY 会把它打坏
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("SAMEORIGIN"),
        ))
}

/// 给 SSE 响应加 `X-Accel-Buffering: no`（见上方的调用处注释）。
async fn disallow_proxy_buffering(
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    let is_sse = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    if is_sse {
        response.headers_mut().insert(
            axum::http::header::HeaderName::from_static("x-accel-buffering"),
            axum::http::header::HeaderValue::from_static("no"),
        );
    }
    response
}

/// 首页 HTML 的 CSP：主站资源全部自托管（脚本/字体走 'self'，图片额外
/// 允许 data:/blob: 内联，媒体与 worker 允许 blob:），不放宽到任意外源。
/// 样式保留 'unsafe-inline'：Vue 运行时会注入内联样式。
const HTML_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; media-src 'self' blob:; \
     worker-src 'self' blob:; object-src 'none'; base-uri 'self'; frame-ancestors 'self'";

/// 只给 HTML 响应加 CSP（`/` 的 index.html）。内容类型不是 HTML 的响应
/// （如 ServeFile 的 404）不动，CSP 也不加到 /assets 的 JS/CSS 上。
async fn apply_html_csp(
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    let is_html = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/html"));
    if is_html {
        response.headers_mut().insert(
            axum::http::header::CONTENT_SECURITY_POLICY,
            axum::http::HeaderValue::from_static(HTML_CSP),
        );
    }
    response
}

/// 给 EPUB 的 SVG 资产补沙箱 CSP（策略与判定由 service::local_epub_book
/// 提供）：SVG 以 image/svg+xml 同源直出，直接打开时内嵌脚本会在主站
/// 源下执行。CSP 关掉所有资源加载与脚本，只保留 `<img>` 渲染与内联样式；
/// 不改成 Content-Disposition: attachment——那会破坏 EPUB 插画。
async fn sandbox_epub_svg_assets(
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    let is_svg = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(crate::service::local_epub_book::needs_sandbox_csp);
    if is_svg {
        response.headers_mut().insert(
            axum::http::header::CONTENT_SECURITY_POLICY,
            axum::http::HeaderValue::from_static(crate::service::local_epub_book::SVG_ASSET_CSP),
        );
    }
    response
}
