//! `/v1/auth`：登录、刷新、登出、改密。
//!
//! Cookie 语义（契约 §6.1，一次到位）：`SameSite=Lax`；access `Path=/`
//! （覆盖 `/reader3` 过渡期的业务接口，两套命名空间共用同一枚会话）；
//! refresh 收紧到 `/v1/auth`（子路径部署从 `COOKIE_PATH` 推导同前缀）。
//! 响应体保留 `accessToken` 供 Bearer 客户端（§6.2），
//! `refreshToken` 永不进 body。CSRF：`/v1` 全部非 GET 要求
//! `X-Requested-With`（Bearer 在场豁免）。
//!
//! 失败形态：登录失败统一 401 `INVALID_CREDENTIALS`（不区分账号与密码）；
//! 限速 429 `RATE_LIMITED` + `Retry-After`；刷新失败按原因细分
//! `AUTH_EXPIRED` / `AUTH_REFRESH_REPLAYED`，并附带清除 Cookie 的头。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::V1Json;
use crate::api::AppState;
use crate::auth::cookie::{
    build_cookie, clear_cookie, cookie_secure, cookie_value, host_header_value, v1_refresh_path,
    V1_ACCESS_PATH,
};
use crate::auth::cookie::{ACCESS_COOKIE, REFRESH_COOKIE};
use crate::error::error::AppError;
use crate::service::user_service::RefreshFailure;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 登录/刷新/登出：不需要 access 会话（登录是获取会话的动作，刷新只凭
/// refresh Cookie，登出幂等清 Cookie），但都受 CSRF 中间件约束。
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/login", post(login))
        .route("/v1/auth/refresh", post(refresh))
        .route("/v1/auth/logout", post(logout))
}

/// 改密：需要 access 会话（挂在 `/v1` 的 require_auth 之后）。
pub fn authed_router() -> Router<AppState> {
    Router::new().route("/v1/auth/password", post(change_password))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangePasswordRequest {
    pub current_password: Option<String>,
    pub new_password: Option<String>,
}

/// 会话令牌对。`accessToken` 供非浏览器客户端走 `Authorization: Bearer`；
/// 浏览器只依赖 HttpOnly Cookie，`refreshToken` 永不进响应体。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTokens {
    pub access_token: String,
    /// access token 的过期时间，RFC 3339 UTC
    #[schema(format = DateTime)]
    pub access_token_expires_at: String,
    /// 登录用户名（hint 展示与 API 客户端识别；/v1/me 才是资料源）
    pub username: String,
}

fn v1_cookie_paths(config: &crate::app::config::AppConfig) -> (&'static str, String) {
    (V1_ACCESS_PATH, v1_refresh_path(&config.cookie_path))
}

/// 登录/刷新/改密成功：双 Cookie（v1 路径语义）+ 令牌对。
fn v1_session_response(
    state: &AppState,
    headers: &HeaderMap,
    session: crate::service::user_service::IssuedSession,
) -> Response {
    let access_token = session.data["accessToken"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let expires_at = crate::api::v1::me::rfc3339_secs(
        crate::util::time::now_ts() + state.config.access_token_ttl_secs as i64,
    );
    let secure = cookie_secure(&state.config.cookie_secure, host_header_value(headers));
    let (access_path, refresh_path) = v1_cookie_paths(&state.config);
    let mut cookies = HeaderMap::new();
    cookies.append(
        header::SET_COOKIE,
        build_cookie(
            ACCESS_COOKIE,
            &access_token,
            state.config.access_token_ttl_secs,
            access_path,
            secure,
        ),
    );
    cookies.append(
        header::SET_COOKIE,
        build_cookie(
            REFRESH_COOKIE,
            &session.refresh_token,
            state.config.refresh_token_ttl_secs,
            &refresh_path,
            secure,
        ),
    );
    (
        cookies,
        Json(SessionTokens {
            access_token,
            access_token_expires_at: expires_at,
            username: session.data["username"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        }),
    )
        .into_response()
}

/// 清除两枚 Cookie（`Max-Age=0`，路径与下发时一致）的响应头。
fn v1_cleared_cookie_headers(state: &AppState, headers: &HeaderMap) -> HeaderMap {
    let secure = cookie_secure(&state.config.cookie_secure, host_header_value(headers));
    let (access_path, refresh_path) = v1_cookie_paths(&state.config);
    let mut cookies = HeaderMap::new();
    cookies.append(
        header::SET_COOKIE,
        clear_cookie(ACCESS_COOKIE, access_path, secure),
    );
    cookies.append(
        header::SET_COOKIE,
        clear_cookie(REFRESH_COOKIE, &refresh_path, secure),
    );
    cookies
}

/// 刷新失败的 401 problem：附清除 Cookie 的头（浏览器无感知地退出会话）。
fn cleared_problem_response(state: &AppState, headers: &HeaderMap, problem: Problem) -> Response {
    let mut response = problem.into_response();
    for value in v1_cleared_cookie_headers(state, headers).values() {
        response
            .headers_mut()
            .append(header::SET_COOKIE, value.clone());
    }
    response
}

/// 登录。
///
/// 失败统一 401 `INVALID_CREDENTIALS`（不区分账号不存在与密码错误）；
/// 限速命中回 429 `RATE_LIMITED` + `Retry-After`。
#[utoipa::path(
    post,
    path = "/v1/auth/login",
    tag = "auth",
    operation_id = "login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "登录成功，Set-Cookie 下发会话", body = SessionTokens),
        (status = 401, description = "用户名或密码错误",
         body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "登录失败次数过多",
         body = Problem, content_type = "application/problem+json"),
        (status = 400, description = "请求体不是合法 JSON 或字段类型不符",
         body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "请求体超过 1MB 上限",
         body = Problem, content_type = "application/problem+json"),
        (status = 415, description = "Content-Type 不是 application/json",
         body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    V1Json(req): V1Json<LoginRequest>,
) -> Result<Response, Problem> {
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    // 限速按客户端 IP 记账：对端命中 TRUSTED_PROXIES 时从 X-Forwarded-For
    // 还原真实地址（与 /reader3 登录同一套口径）
    let trusted = crate::util::client_ip::parse_or_default(&state.config.trusted_proxies);
    let client_ip = crate::util::client_ip::resolve_client_ip(addr.ip(), &headers, &trusted);
    let session = match state
        .user_service
        .login(&username, &password, Some(&client_ip))
        .await
    {
        Ok(session) => session,
        Err(AppError::TooManyRequests {
            message,
            retry_after_secs,
        }) => {
            return Err(Problem::new(ErrorCode::RateLimited)
                .detail(message)
                .retry_after(retry_after_secs));
        }
        // 契约：登录失败统一 401，不区分账号不存在与密码错误（文案一致，
        // 服务端已在 service 层做等时校验防时序枚举）
        Err(_) => {
            return Err(Problem::new(ErrorCode::InvalidCredentials).detail("用户名或密码错误"));
        }
    };
    Ok(v1_session_response(&state, &headers, session))
}

/// 用一次性 Refresh Token 换一对新令牌（轮换 + 30 秒宽限 + 重放吊销家族）。
///
/// 只认 `reader_refresh` Cookie。失败按原因细分并清除两枚 Cookie：
/// `AUTH_EXPIRED`（无效/过期，静默失败）与 `AUTH_REFRESH_REPLAYED`
/// （重放检测，会话已吊销，必须重新登录）。
#[utoipa::path(
    post,
    path = "/v1/auth/refresh",
    tag = "auth",
    operation_id = "refreshToken",
    responses(
        (status = 200, description = "换发成功，Set-Cookie 下发新令牌对", body = SessionTokens),
        (status = 401, description = "令牌无效/过期或重放检测命中，附清除 Cookie",
         body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn refresh(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(token) = cookie_value(&headers, REFRESH_COOKIE) else {
        return cleared_problem_response(
            &state,
            &headers,
            Problem::new(ErrorCode::AuthExpired).detail("会话已过期，请重新登录"),
        );
    };
    match state.user_service.refresh_session_detailed(&token).await {
        Ok(session) => v1_session_response(&state, &headers, session),
        Err(RefreshFailure::Invalid) => cleared_problem_response(
            &state,
            &headers,
            Problem::new(ErrorCode::AuthExpired).detail("会话已过期，请重新登录"),
        ),
        Err(RefreshFailure::Replayed) => cleared_problem_response(
            &state,
            &headers,
            Problem::new(ErrorCode::AuthRefreshReplayed)
                .detail("检测到令牌重放，会话已吊销，请重新登录"),
        ),
    }
}

/// 登出：吊销 refresh 会话家族并清除两枚 Cookie。幂等——Cookie 缺失或
/// 令牌已损坏同样返回 204。
#[utoipa::path(
    post,
    path = "/v1/auth/logout",
    tag = "auth",
    operation_id = "logout",
    responses((status = 204, description = "已登出并清除 Cookie"))
)]
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_value(&headers, REFRESH_COOKIE) {
        if let Err(error) = state.user_service.revoke_refresh_token(&token).await {
            tracing::warn!("登出时吊销 Refresh Token 失败: {error}");
        }
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    for value in v1_cleared_cookie_headers(&state, &headers).values() {
        response
            .headers_mut()
            .append(header::SET_COOKIE, value.clone());
    }
    response
}

/// 修改密码：吊销其他会话（token_version 自增），当前设备用换发的新令牌
/// 继续使用，避免改完密码被登出。
#[utoipa::path(
    post,
    path = "/v1/auth/password",
    tag = "auth",
    operation_id = "changePassword",
    request_body = ChangePasswordRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已修改并换发当前会话"),
        (status = 401, description = "未登录或当前密码错误",
         body = Problem, content_type = "application/problem+json"),
        (status = 400, description = "请求体不是合法 JSON 或字段类型不符",
         body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "请求体超过 1MB 上限",
         body = Problem, content_type = "application/problem+json"),
        (status = 415, description = "Content-Type 不是 application/json",
         body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    user: crate::api::v1::V1User,
    V1Json(req): V1Json<ChangePasswordRequest>,
) -> Result<Response, Problem> {
    let current = req.current_password.unwrap_or_default();
    let new = req.new_password.unwrap_or_default();
    if current.is_empty() || new.is_empty() {
        return Err(Problem::new(ErrorCode::ValidationFailed).detail("请填写当前密码和新密码"));
    }
    let session = state
        .user_service
        .change_password(&user.0.username, &current, &new)
        .await
        .map_err(|error| {
            // 当前密码错误沿用服务的 401 语义：与「凭据失效」同形态，
            // 前端只展示 detail
            match error {
                AppError::BadRequest(message) => {
                    Problem::new(ErrorCode::InvalidCredentials).detail(message)
                }
                other => Problem::from(other),
            }
        })?;
    // 204 + 换发的新 Cookie 对（无响应体；契约口径）
    let access_token = session.data["accessToken"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let secure = cookie_secure(&state.config.cookie_secure, host_header_value(&headers));
    let (access_path, refresh_path) = v1_cookie_paths(&state.config);
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        build_cookie(
            ACCESS_COOKIE,
            &access_token,
            state.config.access_token_ttl_secs,
            access_path,
            secure,
        ),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        build_cookie(
            REFRESH_COOKIE,
            &session.refresh_token,
            state.config.refresh_token_ttl_secs,
            &refresh_path,
            secure,
        ),
    );
    Ok(response)
}
