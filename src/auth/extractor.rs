use crate::error::error::{AppError, NEED_LOGIN};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::convert::Infallible;

/// 已认证用户，由 [`super::middleware::require_auth`] / `optional_auth` 写入请求扩展。
///
/// 各 handler 通过 [`CurrentUser`] / [`MaybeUser`] 取用，不再自行解析凭据。
/// 单用户部署下没有角色区分：能拿到 AuthUser 即拥有全部能力。
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub username: String,
    /// 用户命名空间；所有按用户隔离的存储都以它为路径前缀
    pub ns: String,
    /// 令牌里的撤销版本号（`users.token_version`），供铸造派生令牌时沿用
    pub ver: i64,
    /// 限定用途（空 = 全功能主令牌），见 `auth::jwt::PROXY_TOKEN_SCOPE`
    pub scope: Option<String>,
    /// 代理令牌绑定的书源（`bookSourceUrl`）
    pub proxy_source: Option<String>,
}

/// 必须已登录；中间件未写入身份时返回 401。
#[derive(Debug, Clone)]
pub struct CurrentUser(pub AuthUser);

#[axum::async_trait]
impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthUser>()
            .cloned()
            .map(CurrentUser)
            .ok_or_else(unauthorized)
    }
}

/// 可选身份：未携带令牌或令牌无效时为 `None`，不报错。
///
/// 供 `getUserInfo` 这类「未登录也要能调用」的端点使用。
#[derive(Debug, Clone, Default)]
pub struct MaybeUser(pub Option<AuthUser>);

#[axum::async_trait]
impl<S> FromRequestParts<S> for MaybeUser
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(MaybeUser(parts.extensions.get::<AuthUser>().cloned()))
    }
}

fn unauthorized() -> AppError {
    AppError::Unauthorized(NEED_LOGIN.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    fn user() -> AuthUser {
        AuthUser {
            username: "reader1".to_string(),
            ns: "reader1".to_string(),
            ver: 1,
            scope: None,
            proxy_source: None,
        }
    }

    #[tokio::test]
    async fn current_user_reads_identity_from_extensions() {
        let mut request = Request::new(());
        request.extensions_mut().insert(user());
        let (mut parts, _) = request.into_parts();
        let CurrentUser(extracted) = CurrentUser::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(extracted.ns, "reader1");
    }

    #[tokio::test]
    async fn current_user_rejects_when_middleware_did_not_run() {
        let request = Request::new(());
        let (mut parts, _) = request.into_parts();
        let err = CurrentUser::from_request_parts(&mut parts, &())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(message) if message == NEED_LOGIN));
    }

    #[tokio::test]
    async fn maybe_user_is_none_without_identity() {
        let request = Request::new(());
        let (mut parts, _) = request.into_parts();
        let MaybeUser(extracted) = MaybeUser::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert!(extracted.is_none());
    }
}
