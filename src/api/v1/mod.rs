//! `/v1` 命名空间：RESTful 契约接口（Draft-2）。
//!
//! 与 `/reader3` 按域切换并存（见 goal 决策 D3）：每个域的 v1 端点落在
//! `v1/` 下的子模块，复用现有 service 层；响应一律具名 schema + problem+json
//! 错误，`ApiResponse` 不进入本命名空间。鉴权中间件独立于 `/reader3` 的
//! `require_auth`——同样的令牌校验逻辑，失败返回 problem+json（401 +
//! `AUTH_EXPIRED`）而不是 `ApiResponse`。
//!
//! 分层（请求自外向内）：并发上限（与 `/reader3` 共享同一信号量）→ 入站
//! 超时 → traceId 补齐 → 鉴权 → handler。traceId 中间件对所有 problem 响应
//! 统一盖章（契约 §5.7：每个错误体都有），handler 无需自行处理。
//!
//! OpenAPI 文档由代码生成：`openapi_yaml()` 的输出与仓库根目录
//! `openapi.yaml` 做漂移检查（`tests/openapi_drift.rs`），不一致即失败；
//! 更新锚文件用 `UPDATE_OPENAPI=1 cargo test --test openapi_drift`。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::AppState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

mod auth;
mod books;

mod content;
mod me;
mod replace_rules;
mod shelf;
mod sources;

/// `/v1` 路由。并发层与 `/reader3` 的受保护组共享同一份许可
/// （`GlobalConcurrencyLimitLayer` clone 共享内部信号量），两个命名空间
/// 共用同一个全局并发预算；`concurrency` 为 `None` 表示不限并发。
pub fn router(
    state: AppState,
    inbound_timeout: std::time::Duration,
    concurrency: Option<tower::limit::GlobalConcurrencyLimitLayer>,
) -> Router {
    let v1 = Router::new()
        .merge(me::router())
        .merge(replace_rules::router())
        .merge(shelf::router())
        .merge(books::router())
        .merge(content::router())
        .merge(sources::router())
        .merge(auth::authed_router())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth,
        ))
        // 公开认证端点（登录/刷新/登出）在鉴权层之外，同样受 CSRF 约束
        .merge(auth::public_router())
        // /v1 的请求体上限：现有端点都是小 JSON（登录/改密/资料），按 1MB
        // 缓冲而不是最外层兜底的 100MB——大 body 域（章节导入等）在各自
        // 路由上挂更大的独立上限。Router::layer 只作用于已添加的路由，
        // 必须放在全部 merge 之后
        .layer(axum::extract::DefaultBodyLimit::max(
            crate::api::router::PUBLIC_BODY_LIMIT_BYTES,
        ));
    let v1 = match concurrency {
        Some(layer) => v1.layer(layer),
        None => v1,
    };
    v1.layer(axum::middleware::from_fn(require_csrf))
        .layer(axum::middleware::from_fn_with_state(
            (
                inbound_timeout,
                crate::api::inbound_guard::TimeoutShape::Problem,
            ),
            crate::api::inbound_guard::timeout_middleware,
        ))
        .layer(axum::middleware::from_fn(attach_problem_trace))
        .with_state(state)
}

/// 未注册的 `/v1` 路径：problem+json 404（`/v1` 是纯 API 命名空间，
/// 不落到静态文件服务）。
pub async fn not_found() -> Problem {
    Problem::new(ErrorCode::NotFound).detail("接口不存在")
}

/// `/v1` 的强制鉴权：逻辑与 `auth::middleware::require_auth` 一致
/// （同一套令牌校验与撤销检查），失败形态换成 problem+json。
/// CSRF 头检查与 Cookie 语义调整随 auth 域切换落地，当前只挂 `/v1`。
async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, Problem> {
    use crate::auth::middleware::{authenticate, extract_token};
    let token = extract_token(&request)
        .ok_or_else(|| Problem::new(ErrorCode::AuthExpired).detail("未登录或令牌已过期"))?;
    let user = authenticate(&state.auth, token)
        .await
        .map_err(Problem::from)?;
    // 限定用途令牌（书源登录代理）只服务 /reader3 代理路径，不进 /v1
    if user.scope.is_some() {
        return Err(Problem::new(ErrorCode::ScopeForbidden).detail("限定令牌不能访问 /v1 接口"));
    }
    request.extensions_mut().insert(user);
    Ok(next.run(request).await)
}

/// CSRF 双保险：`/v1` 的全部非 GET 请求要求 `X-Requested-With:
/// XMLHttpRequest`（跨站表单无法设置自定义头）。携带 `Authorization`
/// 的请求豁免（Bearer 客户端不经 Cookie 会话，天然免疫 CSRF）。
/// `SameSite=Lax` 挡掉跨站非 GET 的 Cookie 携带，这里补上第二道。
async fn require_csrf(request: Request, next: Next) -> Response {
    let is_safe = matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    );
    let has_bearer = request.headers().get(header::AUTHORIZATION).is_some();
    if is_safe || has_bearer {
        return next.run(request).await;
    }
    let marked = request
        .headers()
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("XMLHttpRequest"));
    if marked {
        return next.run(request).await;
    }
    Problem::new(ErrorCode::CsrfHeaderMissing)
        .detail("缺少 X-Requested-With 头")
        .into_response()
}

/// problem 体统一补 `traceId`：与响应头 `X-Request-Id` 一致（用户截图能
/// 定位日志）。只处理 problem+json 的错误响应，资源响应原样透传；改写只在
/// 错误路径发生，代价可忽略。handler 自行构造的 problem 无需关心 traceId。
async fn attach_problem_trace(request: Request, next: Next) -> Response {
    let trace_id = request
        .extensions()
        .get::<tower_http::request_id::RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .map(str::to_string);
    let response = next.run(request).await;

    let is_error = response.status().is_client_error() || response.status().is_server_error();
    if trace_id.is_none() || !is_error {
        return response;
    }
    let is_problem = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/problem+json"));
    if !is_problem {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    // problem 体都很小；超限（理论不可达）退回通用 INTERNAL，不带半截改写
    let Ok(bytes) = axum::body::to_bytes(body, 64 * 1024).await else {
        return Problem::new(ErrorCode::Internal)
            .detail("internal error")
            .trace(trace_id.expect("checked above"))
            .into_response();
    };
    let mut value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value @ serde_json::Value::Object(_)) => value,
        _ => return Response::from_parts(parts, Body::from(bytes)),
    };
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "traceId".to_string(),
            serde_json::Value::String(trace_id.expect("checked above")),
        );
    }
    let rewritten = serde_json::to_vec(&value).unwrap_or_else(|_| bytes.to_vec());
    // 长度变化后交给 hyper 按新 body 重算，移除旧头避免不一致
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(rewritten))
}

/// `/v1` 的 JSON 提取器：rejection 是 problem+json。
/// axum 的 `Json<T>` 默认 rejection 是纯文本（400/413/415），前端拿不到
/// `code` 只能退回通用文案；`/v1` 的每一个 body 解析失败都应是可派发的
/// problem。映射：413 → PAYLOAD_TOO_LARGE、415 → UNSUPPORTED_MEDIA_TYPE、
/// 其余（语法错误/类型不匹配）→ MALFORMED_REQUEST 400。
#[derive(Debug, Clone, Copy, Default)]
pub struct V1Json<T>(pub T);

impl<T, S> axum::extract::FromRequest<S> for V1Json<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        use axum::extract::rejection::JsonRejection;
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(V1Json(value)),
            Err(rejection) => {
                let text = rejection.body_text();
                let (code, detail) = match rejection {
                    JsonRejection::MissingJsonContentType(_) => (
                        ErrorCode::UnsupportedMediaType,
                        "Content-Type 必须为 application/json",
                    ),
                    ref other if other.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE => {
                        (ErrorCode::PayloadTooLarge, "请求体超过大小上限")
                    }
                    // deny_unknown_fields 的未知字段：契约要求 422 UNKNOWN_FIELD
                    // （book-fields §4），从错误文本识别（serde 无结构化变体）
                    ref other
                        if other.status() == axum::http::StatusCode::UNPROCESSABLE_ENTITY
                            || text.contains("unknown field") =>
                    {
                        (ErrorCode::UnknownField, "请求包含未知或不可写的字段")
                    }
                    _ => (
                        ErrorCode::MalformedRequest,
                        "请求体不是合法 JSON 或字段类型不符",
                    ),
                };
                Err(Problem::new(code).detail(detail))
            }
        }
    }
}

/// `/v1` 的身份提取器：rejection 是 `Problem`，杜绝 `ApiResponse` 形态
/// 泄漏——即使某条 `/v1` 路由漏挂鉴权中间件，失败形态仍是 problem+json。
#[derive(Debug, Clone)]
pub struct V1User(pub crate::auth::extractor::AuthUser);

impl axum::extract::FromRequestParts<AppState> for V1User {
    type Rejection = Problem;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<crate::auth::extractor::AuthUser>()
            .cloned()
            .map(V1User)
            .ok_or_else(|| Problem::new(ErrorCode::AuthExpired).detail("未登录或令牌已过期"))
    }
}

/// OpenAPI 安全方案：浏览器走会话 Cookie，API 客户端走 Bearer（契约 §6.1/§6.2）。
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "cookieAuth",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "reader_access",
                "会话 Cookie（HttpOnly），登录后由服务端下发",
            ))),
        );
        components.add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
        );
    }
}

/// 由代码生成的 OpenAPI 文档（YAML）。`tests/openapi_drift.rs` 用它与
/// 仓库根目录的 `openapi.yaml` 做一致性检查。
pub fn openapi_yaml() -> String {
    V1ApiDoc::openapi()
        .to_yaml()
        .expect("openapi yaml serialization is infallible for static schemas")
}

#[derive(OpenApi)]
#[openapi(
    info(title = "reader-rust v1 API", version = "0.1.0"),
    paths(
        me::get_me,
        books::list_chapters,
        content::get_chapter_content,
        content::batch_chapter_content,
        sources::list_sources,
        shelf::add_to_shelf,
        shelf::patch_shelf_book,
        shelf::remove_from_shelf,
        shelf::get_book,
        replace_rules::list_replace_rules,
        replace_rules::create_replace_rule,
        replace_rules::patch_replace_rule,
        replace_rules::delete_replace_rule,
        replace_rules::batch_upsert_replace_rules,
        replace_rules::batch_delete_replace_rules,
        shelf::patch_book,
        auth::login,
        auth::refresh,
        auth::logout,
        auth::change_password,
    ),
    components(schemas(
        me::Me,
        Problem,
        ErrorCode,
        crate::api::problem::FieldIssue,
        auth::LoginRequest,
        auth::ChangePasswordRequest,
        auth::SessionTokens,
        sources::SourceSummary,
        sources::SourcePage,
        books::ChapterItem,
        books::ChapterPage,
        content::ChapterContent,
        content::ChapterVersionInfo,
        content::BatchContentRequest,
        content::BatchContentResponse,
        content::BatchContentItem,
        content::BatchContentError,
        crate::service::chapter_diff_service::DiffOp,
        shelf::ShelfWriteResult,
        shelf::ShelfPatch,
        shelf::ProgressPatch,
        shelf::BookPatch,
        replace_rules::ReplaceRuleItem,
        replace_rules::ReplaceRuleWrite,
        replace_rules::ReplaceRulePatch,
        replace_rules::BatchUpsertRequest,
        replace_rules::BatchDeleteRequest,
        shelf::BookDetail,
        shelf::LastChapter,
    )),
    tags(
        (name = "me", description = "当前账号"),
        (name = "auth", description = "会话认证"),
        (name = "sources", description = "书源"),
        (name = "books", description = "书籍与章节"),
    ),
    modifiers(&SecurityAddon)
)]
struct V1ApiDoc;
