use crate::api::AppState;
use crate::auth::extractor::AuthUser;
use crate::auth::jwt::decode_token;
use crate::auth::{is_valid_user_ns, AuthState};
use crate::error::error::{AppError, FORBIDDEN, NEED_LOGIN};
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

/// 强制鉴权：缺失/无效/过期令牌，或用户已被删除、撤销版本不匹配，一律 401。
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let token = extract_token(&request).ok_or_else(unauthorized)?;
    let user = authenticate(&state.auth, token).await?;
    request.extensions_mut().insert(user);
    Ok(next.run(request).await)
}

/// 管理员专用：先认证，再校验角色；非管理员一律 403。
pub async fn require_admin(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let token = extract_token(&request).ok_or_else(unauthorized)?;
    let user = authenticate(&state.auth, token).await?;
    if !user.is_admin {
        return Err(AppError::Forbidden(FORBIDDEN.to_string()));
    }
    request.extensions_mut().insert(user);
    Ok(next.run(request).await)
}

/// 可选鉴权：携带有效令牌时注入身份，否则静默放行。
///
/// 供 `getUserInfo` 这类「未登录也要能调用、只是返回空」的端点使用。
pub async fn optional_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    if let Some(token) = extract_token(&request) {
        if let Ok(user) = authenticate(&state.auth, token).await {
            request.extensions_mut().insert(user);
        }
    }
    Ok(next.run(request).await)
}

/// 令牌以所有权传入而非借用 `Request`：跨 `.await` 持有请求借用会让
/// axum 无法推导中间件的 extractor 元组，`from_fn_with_state` 直接编译失败。
async fn authenticate(state: &AuthState, token: String) -> Result<AuthUser, AppError> {
    let claims = decode_token(&token, state.jwt_secret())?;
    let identity = state
        .load_identity(&claims.sub)
        .await?
        .ok_or_else(unauthorized)?;
    // 撤销检查：改密码/重置/删号会自增 token_version，使旧令牌立即失效
    if identity.token_version != claims.ver {
        return Err(unauthorized());
    }
    let ns = resolve_ns(&claims.ns, &claims.sub);
    Ok(AuthUser {
        username: claims.sub,
        ns,
        token,
        is_admin: identity.is_admin,
        enable_webdav: identity.enable_webdav,
        enable_ai_model: identity.enable_ai_model,
    })
}

/// 令牌从 `Authorization` 头取，回退到查询参数 `accessToken`。
///
/// 查询参数不是可选的兼容路径：SSE 端点由浏览器 `EventSource` 发起，
/// 无法设置请求头，前端只能把令牌放进查询串
/// （见 `frontend/src/utils/secureAccess.ts`）。`/reader3/cover` 的
/// `<img src>` 同理。
fn extract_token<B>(request: &axum::http::Request<B>) -> Option<String> {
    if let Some(raw) = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        if let Some(token) = parse_bearer(raw) {
            return Some(token);
        }
    }
    let query = request.uri().query()?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "accessToken")
        .map(|(_, value)| value.into_owned())
        .filter(|value| !value.trim().is_empty())
}

fn parse_bearer(raw: &str) -> Option<String> {
    // 只去前导空白：尾部空白要留给 split_once，否则 "Bearer   " 会被
    // 压成 "Bearer" 而被当成一个裸令牌
    let value = raw.trim_start();
    if value.is_empty() {
        return None;
    }
    match value.split_once(' ') {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("bearer") => {
            let token = rest.trim();
            (!token.is_empty()).then(|| token.to_string())
        }
        // 兼容裸令牌（不带 Bearer 前缀）
        _ => {
            let token = value.trim();
            (!token.is_empty()).then(|| token.to_string())
        }
    }
}

/// 命名空间参与 storage 路径拼接，异常值一律退回用户名。
///
/// 令牌已验签，正常情况下 `ns` 就是用户名；这里只是兜底，防止将来
/// 有别的签发路径写入非法值。退回用户名而不是空串：空串会塌缩到
/// data 根目录（跨用户共享区）。
fn resolve_ns(ns: &str, username: &str) -> String {
    let ns = ns.trim();
    if is_valid_user_ns(ns) {
        ns.to_string()
    } else {
        username.trim().to_string()
    }
}

fn unauthorized() -> AppError {
    AppError::Unauthorized(NEED_LOGIN.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    fn request_with_header(value: &str) -> Request<()> {
        let mut request = Request::new(());
        request
            .headers_mut()
            .insert(AUTHORIZATION, value.parse().unwrap());
        request
    }

    #[test]
    fn parses_bearer_and_bare_tokens() {
        assert_eq!(
            parse_bearer("Bearer abc.def.ghi").as_deref(),
            Some("abc.def.ghi")
        );
        assert_eq!(
            parse_bearer("bearer abc.def.ghi").as_deref(),
            Some("abc.def.ghi")
        );
        // 方案名大小写不敏感
        assert_eq!(
            parse_bearer("BEARER abc.def.ghi").as_deref(),
            Some("abc.def.ghi")
        );
        assert_eq!(parse_bearer("abc.def.ghi").as_deref(), Some("abc.def.ghi"));
        // 只有方案名、没有令牌
        assert_eq!(parse_bearer("Bearer    "), None);
        assert_eq!(parse_bearer("Bearer"), Some("Bearer".to_string()));
        assert_eq!(parse_bearer("   "), None);
    }

    #[test]
    fn reads_token_from_authorization_header() {
        let request = request_with_header("Bearer token-from-header");
        assert_eq!(
            extract_token(&request).as_deref(),
            Some("token-from-header")
        );
    }

    #[test]
    fn falls_back_to_query_parameter_for_event_source() {
        let request = Request::builder()
            .uri("/reader3/searchBookMultiSSE?key=x&accessToken=token-from-query")
            .body(())
            .unwrap();
        assert_eq!(extract_token(&request).as_deref(), Some("token-from-query"));
    }

    #[test]
    fn header_wins_over_query_parameter() {
        let mut request = request_with_header("token-from-header");
        *request.uri_mut() = "/reader3/x?accessToken=token-from-query".parse().unwrap();
        assert_eq!(
            extract_token(&request).as_deref(),
            Some("token-from-header")
        );
    }

    #[test]
    fn blank_query_token_is_ignored() {
        let request = Request::builder()
            .uri("/reader3/x?accessToken=%20")
            .body(())
            .unwrap();
        assert_eq!(extract_token(&request), None);
    }

    #[test]
    fn missing_token_yields_none() {
        let request = Request::builder().uri("/reader3/x").body(()).unwrap();
        assert_eq!(extract_token(&request), None);
    }

    #[test]
    fn resolve_ns_falls_back_to_username_for_invalid_claim() {
        assert_eq!(resolve_ns("reader1", "reader1"), "reader1");
        // 非法 ns 退回用户名而不是空串，保住用户隔离
        assert_eq!(resolve_ns("", "reader1"), "reader1");
        assert_eq!(resolve_ns("../etc", "reader1"), "reader1");
    }
}
