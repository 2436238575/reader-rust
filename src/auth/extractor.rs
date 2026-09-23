use crate::error::error::{AppError, NEED_LOGIN};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::convert::Infallible;

/// 已认证用户，由 [`super::middleware::require_auth`] / `optional_auth` 写入请求扩展。
///
/// 各 handler 通过 [`CurrentUser`] / [`MaybeUser`] 取用，不再自行解析凭据；
/// 管理员端点则由 `require_admin` 中间件在路由层拦截。
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub username: String,
    /// 用户命名空间；所有按用户隔离的存储都以它为路径前缀
    pub ns: String,
    /// 本次请求使用的原始令牌。`bookSourceProxy` 需要把它注入被代理的
    /// 页面，好让页面自身的后续请求继续带着身份回来。
    pub token: String,
    pub is_admin: bool,
    pub enable_webdav: bool,
    pub enable_ai_model: bool,
}

impl AuthUser {
    /// WebDAV 的按用户开关；未开启一律 403。
    pub fn require_webdav_ns(&self) -> Result<&str, AppError> {
        if !self.enable_webdav {
            return Err(AppError::Forbidden("未开启webdav功能".to_string()));
        }
        Ok(&self.ns)
    }

    /// 服务端 AI 模型配置的使用权限。
    pub fn can_use_ai_model(&self) -> bool {
        self.is_admin || self.enable_ai_model
    }
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

    fn user(is_admin: bool) -> AuthUser {
        AuthUser {
            username: "reader1".to_string(),
            ns: "reader1".to_string(),
            token: "test-token".to_string(),
            is_admin,
            enable_webdav: true,
            enable_ai_model: false,
        }
    }

    #[tokio::test]
    async fn current_user_reads_identity_from_extensions() {
        let mut request = Request::new(());
        request.extensions_mut().insert(user(false));
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

    #[test]
    fn webdav_switch_and_ai_permission() {
        assert!(user(true).require_webdav_ns().is_ok());
        assert!(user(true).can_use_ai_model());

        let mut disabled = user(false);
        disabled.enable_webdav = false;
        assert!(disabled.require_webdav_ns().is_err());
        assert!(!disabled.can_use_ai_model());
    }
}
