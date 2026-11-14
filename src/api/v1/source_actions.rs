//! `/v1/sources/{sourceId}` 动作：执行登录规则、签发书源代理限定令牌。
//!
//! - `POST /v1/sources/{sourceId}/login`：跑 loginUrl + loginCheckJs，成功
//!   落 Cookie；返回 `{ok, message, cookieCount, previewUrl?}`。失败 422
//!   `SOURCE_LOGIN_FAILED`，只给可读原因，**不回显任何 Cookie 值/响应体**。
//! - `POST /v1/sources/{sourceId}/proxy-token`：10 分钟、scope=bookSourceProxy、
//!   绑定单个 source（bsid）的限定令牌，只能访问 `/reader3/bookSourceProxy`
//!   前缀；主 JWT 不进入 iframe URL。

use crate::api::handlers::book_source::mint_proxy_token_with_ttl;
use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::sources::parse_source_path_id;
use crate::api::v1::V1User;
use crate::api::AppState;
use crate::error::error::AppError;
use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

/// v1 代理令牌 TTL：契约要求 ≤10 分钟。
const PROXY_TOKEN_TTL_SECS: i64 = 10 * 60;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sources/{sourceId}/login", post(login_source))
        .route("/v1/sources/{sourceId}/proxy-token", post(mint_proxy_token))
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceLoginResult {
    pub ok: bool,
    pub message: String,
    /// 本次最终响应 `Set-Cookie` 的条数（不返回 Cookie 值）
    pub cookie_count: u32,
    /// 登录请求的最终页面地址，前端用它打开代理预览（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub preview_url: Option<String>,
}

/// 登录判定：HTTP 成功 + loginCheckJs 没有显式返回失败。
fn check_passed(value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return true;
    };
    match value {
        Value::Null => true,
        Value::Bool(passed) => *passed,
        Value::String(text) => {
            let normalized = text.trim().to_ascii_lowercase();
            !normalized.is_empty() && normalized != "false" && normalized != "0"
        }
        _ => true,
    }
}

/// 执行书源登录规则（loginUrl / loginCheckJs），成功时 Cookie 已写入用户
/// 的独立 Cookie jar。失败返回 422，不泄露 Cookie 值或响应体。
#[utoipa::path(
    post,
    path = "/v1/sources/{sourceId}/login",
    tag = "sources",
    operation_id = "loginSource",
    params(("sourceId" = String, Path, description = "书源 id（src_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "登录成功", body = SourceLoginResult),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书源不存在", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "登录请求失败或 loginCheckJs 判定失败", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn login_source(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(source_id): Path<String>,
) -> Result<Json<SourceLoginResult>, Problem> {
    let numeric = parse_source_path_id(&source_id)?;
    let source = state
        .book_source_service
        .get_by_id(&user.ns, numeric)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| Problem::new(ErrorCode::SourceNotFound).detail("书源不存在"))?;
    let result = state
        .book_service
        .login_book_source(&user.ns, &source)
        .await
        .map_err(|error| {
            // 不拼接 Cookie/响应体；5xx/Internal 给可读的上游失败文案，
            // 不直接透出 "internal error"。
            let detail = match &error {
                AppError::Upstream { status, .. } => {
                    format!("登录执行失败：上游 HTTP {status}")
                }
                AppError::Blocked(message) | AppError::BadRequest(message) => {
                    format!("登录执行失败：{message}")
                }
                _ => "登录执行失败：上游请求失败".to_string(),
            };
            Problem::new(ErrorCode::SourceLoginFailed).detail(detail)
        })?;
    let status = result["status"].as_u64().unwrap_or(0);
    let http_ok = result["isSuccessful"].as_bool().unwrap_or(false);
    let cookie_count = result["cookieCount"].as_u64().unwrap_or(0) as u32;
    let check_ok = check_passed(result.get("checkResult"));
    let preview_url = result["url"].as_str().map(str::to_string);
    let ok = http_ok && check_ok;
    // 不拼接 loginCheckJs 的任意输出：脚本环境能读到登录页响应体，回显
    // 字符串等于把响应体带进 API/日志。
    let message = if ok {
        format!("登录成功（HTTP {status}），本次响应设置 {cookie_count} 个 Cookie")
    } else if !http_ok {
        format!("登录失败（HTTP {status}）")
    } else {
        format!("登录校验未通过（HTTP {status}）")
    };
    if !ok {
        return Err(Problem::new(ErrorCode::SourceLoginFailed).detail(message));
    }
    Ok(Json(SourceLoginResult {
        ok,
        message,
        cookie_count,
        preview_url,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceProxyTokenResult {
    pub token: String,
    pub expires_in: i64,
}

/// 签发书源代理限定令牌：10 分钟、scope=代理、绑定单一 bookSourceId。
#[utoipa::path(
    post,
    path = "/v1/sources/{sourceId}/proxy-token",
    tag = "sources",
    operation_id = "mintSourceProxyToken",
    params(("sourceId" = String, Path, description = "书源 id（src_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "限定令牌", body = SourceProxyTokenResult),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书源不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn mint_proxy_token(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(source_id): Path<String>,
) -> Result<Json<SourceProxyTokenResult>, Problem> {
    let numeric = parse_source_path_id(&source_id)?;
    if state
        .book_source_service
        .get_by_id(&user.ns, numeric)
        .await
        .map_err(Problem::from)?
        .is_none()
    {
        return Err(Problem::new(ErrorCode::SourceNotFound).detail("书源不存在"));
    }
    let token = mint_proxy_token_with_ttl(&state, &user, numeric, PROXY_TOKEN_TTL_SECS)
        .map_err(Problem::from)?;
    Ok(Json(SourceProxyTokenResult {
        token,
        expires_in: PROXY_TOKEN_TTL_SECS,
    }))
}
