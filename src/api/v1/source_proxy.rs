//! `/v1/sources/{sourceId}/proxy`：书源登录页代理（iframe）。
//!
//! 与普通 v1 JSON 接口不同，代理页是第三方书源的 HTML/JS，认证只能靠
//! URL 里的**限定令牌**（scope=bookSourceProxy、绑定单个书源），不能依赖
//! Cookie/Bearer：iframe 被 CSP sandbox 关进不透明源，且主 JWT 不能进
//! iframe URL。因此这个子路由挂在 v1 标准鉴权/CSRF 层之外，自带
//! `require_proxy_token` 语义：只认查询串 `accessToken`，失败 problem+json。
//!
//! 代理转发、Cookie 剔除、HTML/JS URL 重写、CSP sandbox 与出站守卫全部
//! 复用 `handlers/book_source.rs` 的 `proxy_request`；客户端错误上报
//! `client-log` 同样要求限定令牌。
//!
//! 代理返回第三方 HTML/字节流，和 WebDAV 一样**不进入 OpenAPI**（无法用
//! JSON schema 描述）；契约与安全口径以 `docs/api/book-source.md` 为准，
//! 集成测试覆盖鉴权、URL 重写与 CSP。

use crate::api::handlers::book_source::{
    book_source_proxy_request, record_client_log, ProxyInvocation,
};
use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::V1Query;
use crate::api::AppState;
use crate::auth::extractor::AuthUser;
use crate::auth::jwt::PROXY_TOKEN_SCOPE;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

/// 代理页与 client-log 共用的请求体上限：登录表单很小，8MB 与 legacy
/// 受保护组同档；超限在 handler 之前由 DefaultBodyLimit 拒绝。
const PROXY_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

pub fn router(
    state: AppState,
    timeout: std::time::Duration,
    concurrency: Option<tower::limit::GlobalConcurrencyLimitLayer>,
) -> Router<AppState> {
    let router = Router::new()
        .route(
            "/v1/sources/{sourceId}/proxy",
            get(proxy)
                .post(proxy)
                .layer(axum::extract::DefaultBodyLimit::max(PROXY_BODY_LIMIT_BYTES)),
        )
        .route(
            "/v1/sources/{sourceId}/proxy/client-log",
            get(client_log).post(client_log),
        )
        .layer(axum::middleware::from_fn_with_state(
            (timeout, crate::api::inbound_guard::TimeoutShape::Problem),
            crate::api::inbound_guard::timeout_middleware,
        ));
    // 与 /reader3 受保护组、v1 其余端点共享同一份并发预算：代理请求是
    // 用户可触发的出站请求，不能绕过 INBOUND_MAX_CONCURRENCY。
    let router = match concurrency {
        Some(layer) => router.layer(layer),
        None => router,
    };
    router.with_state(state)
}

/// 从 `COOKIE_PATH` 推导部署前缀（`/read/reader3` → `/read`）。
/// 代理页里的 URL 与 Set-Cookie Path 必须带这个前缀，否则子路径部署下
/// 浏览器会跳过 nginx 的 `/read/` location。
fn deployment_prefix(cookie_path: &str) -> &str {
    cookie_path.strip_suffix("/reader3").unwrap_or("")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxyQuery {
    url: Option<String>,
    access_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientLogQuery {
    access_token: Option<String>,
    message: Option<String>,
    source: Option<String>,
    lineno: Option<i64>,
    colno: Option<i64>,
    stack: Option<String>,
}

fn parse_source_id(raw: &str) -> Result<i64, Problem> {
    raw.strip_prefix("src_")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| Problem::new(ErrorCode::SourceNotFound).detail("书源不存在"))
}

/// 只接受绑定到 `source_id` 的限定令牌；不接受 Cookie/Bearer，不要求 CSRF。
async fn require_proxy_token(
    state: &AppState,
    source_id: i64,
    access_token: Option<&str>,
) -> Result<AuthUser, Problem> {
    let token = access_token
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Problem::new(ErrorCode::AuthExpired).detail("缺少代理令牌"))?;
    let user = crate::auth::middleware::authenticate(&state.auth, token.to_string())
        .await
        .map_err(Problem::from)?;
    if user.scope.as_deref() != Some(PROXY_TOKEN_SCOPE) || user.proxy_source != Some(source_id) {
        return Err(Problem::new(ErrorCode::ScopeForbidden).detail("代理令牌与书源不匹配"));
    }
    Ok(user)
}

/// 代理入口：`GET|POST /v1/sources/{sourceId}/proxy?url=&accessToken=`
async fn proxy(
    State(state): State<AppState>,
    Path(source_id): Path<String>,
    V1Query(query): V1Query<ProxyQuery>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Problem> {
    let numeric = parse_source_id(&source_id)?;
    let user = require_proxy_token(&state, numeric, query.access_token.as_deref()).await?;
    let raw_target_url = query
        .url
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Problem::new(ErrorCode::MalformedRequest).detail("url required"))?;
    // 路径身份固定用 src_<n>，与浏览器地址栏/HTML 重写保持同一形态；
    // Cookie Path 也用它，登录页跳转时 Cookie 不会外溢到其他书源。
    // 部署前缀来自 COOKIE_PATH（后端看到的 path 已被反代剥掉前缀，
    // 但写回浏览器的 URL/Cookie Path 必须重新带上）。
    let prefix = deployment_prefix(&state.config.cookie_path);
    let proxy_base = format!("{prefix}/v1/sources/src_{numeric}/proxy");
    let client_log_path = format!("{proxy_base}/client-log");
    book_source_proxy_request(
        &state,
        &user,
        ProxyInvocation {
            source_id: numeric,
            target_url: &raw_target_url,
            method: &method,
            headers: &headers,
            body,
            proxy_base: &proxy_base,
            client_log_path: &client_log_path,
        },
    )
    .await
    .map_err(Problem::from)
}

/// 代理页客户端错误上报：同样要求限定令牌，内容截断 + 按 source 限频。
async fn client_log(
    State(state): State<AppState>,
    Path(source_id): Path<String>,
    V1Query(query): V1Query<ClientLogQuery>,
) -> Result<Json<Value>, Problem> {
    let numeric = parse_source_id(&source_id)?;
    require_proxy_token(&state, numeric, query.access_token.as_deref()).await?;
    let logged = record_client_log(
        numeric,
        query.source.as_deref(),
        query.lineno,
        query.colno,
        query.message.as_deref(),
        query.stack.as_deref(),
    );
    Ok(Json(json!({ "logged": logged })))
}

#[cfg(test)]
mod tests {
    use super::deployment_prefix;

    #[test]
    fn deployment_prefix_follows_cookie_path() {
        assert_eq!(deployment_prefix("/reader3"), "");
        assert_eq!(deployment_prefix("/read/reader3"), "/read");
        assert_eq!(deployment_prefix("/a/b/reader3"), "/a/b");
    }
}
