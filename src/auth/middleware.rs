use crate::api::AppState;
use crate::auth::extractor::AuthUser;
use crate::auth::jwt::decode_token;
use crate::auth::AuthState;
use crate::error::error::{AppError, NEED_LOGIN};
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

/// 强制鉴权：缺失/无效/过期令牌，或撤销版本不匹配，一律 401。
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let token = extract_token(&request).ok_or_else(unauthorized)?;
    let user = authenticate(&state.auth, token).await?;
    // 限定用途令牌（书源登录代理）由 `/v1/sources/{id}/proxy` 子路由自行
    // 校验 query token，不能当主令牌访问任何普通接口。
    if user.scope.is_some() {
        return Err(unauthorized());
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
            // 限定令牌在通用接口上按匿名处理，不注入身份
            if user.scope.is_none() {
                request.extensions_mut().insert(user);
            }
        }
    }
    Ok(next.run(request).await)
}

/// 令牌以所有权传入而非借用 `Request`：跨 `.await` 持有请求借用会让
/// axum 无法推导中间件的 extractor 元组，`from_fn_with_state` 直接编译失败。
pub(crate) async fn authenticate(state: &AuthState, token: String) -> Result<AuthUser, AppError> {
    let claims = decode_token(&token, state.jwt_secret())?;
    let token_version = state
        .load_token_version(claims.uid)
        .await?
        .ok_or_else(unauthorized)?;
    // 撤销检查：改密码会自增 token_version，使旧令牌立即失效
    if token_version != claims.ver {
        return Err(unauthorized());
    }
    Ok(AuthUser {
        uid: claims.uid,
        username: claims.sub,
        ns: user_ns_of(claims.uid),
        ver: claims.ver,
        scope: claims.scope,
        proxy_source: claims.bsid,
    })
}

/// 用户命名空间：`user_<uid>`。username 只用于登录与展示，改它不动数据。
pub fn user_ns_of(uid: i64) -> String {
    format!("user_{uid}")
}

/// 令牌取值顺序：`Authorization: Bearer` 头 → Cookie `reader_access`。
///
/// 浏览器会话走 Cookie（HttpOnly，见 `auth::cookie`）；API 客户端/e2e 走
/// Bearer 头。查询串令牌会进反代访问日志与浏览器历史，普通接口一律不再
/// 接受；书源登录代理的限定令牌由 `/v1/sources/{id}/proxy` 子路由直接从
/// 查询串读取并校验，不经过这里。
pub(crate) fn extract_token<B>(request: &axum::http::Request<B>) -> Option<String> {
    if let Some(raw) = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        if let Some(token) = parse_bearer(raw) {
            return Some(token);
        }
    }
    crate::auth::cookie::cookie_value(request.headers(), crate::auth::cookie::ACCESS_COOKIE)
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

fn unauthorized() -> AppError {
    AppError::Unauthorized(NEED_LOGIN.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, Request};

    fn request_with_header(value: &str) -> Request<()> {
        let mut request = Request::new(());
        request
            .headers_mut()
            .insert(AUTHORIZATION, value.parse().unwrap());
        request
    }

    fn request_with_cookie(value: &str) -> Request<()> {
        let mut request = Request::new(());
        request
            .headers_mut()
            .insert(header::COOKIE, value.parse().unwrap());
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
    fn reads_token_from_access_cookie() {
        let request = request_with_cookie("other=1; reader_access=token-from-cookie");
        assert_eq!(
            extract_token(&request).as_deref(),
            Some("token-from-cookie")
        );
    }

    #[test]
    fn query_token_is_ignored_everywhere() {
        // 查询串令牌只由 /v1 代理子路由自行读取；普通鉴权链一律不认，
        // 避免访问日志/浏览器历史里的令牌被当成主令牌使用。
        for uri in [
            "/reader3/searchBookMultiSSE?key=x&accessToken=token-from-query",
            "/reader3/bookSourceProxy?url=x&accessToken=token-from-query",
            "/v1/me?accessToken=token-from-query",
        ] {
            let request = Request::builder().uri(uri).body(()).unwrap();
            assert_eq!(extract_token(&request), None, "{uri}");
        }
    }

    #[test]
    fn user_ns_of_uses_stable_uid_form() {
        assert_eq!(user_ns_of(1), "user_1");
        assert!(crate::auth::is_valid_user_ns(&user_ns_of(1)));
    }
}
