use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::Value;

use crate::api::AppState;
use crate::auth::extractor::AuthUser;
use crate::auth::{is_valid_user_ns, CurrentUser};
use crate::error::error::{ApiResponse, AppError, FORBIDDEN};
use crate::service::book_service::CacheKind;

/// 清理粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PurgeScope {
    /// 全部用户的全部缓存（仅管理员）
    All,
    /// 某个用户的全部缓存（默认是自己）
    User,
    /// 单本书的缓存
    Book,
    /// 某个用户的某一层缓存
    Kind,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeCacheRequest {
    pub scope: Option<PurgeScope>,
    /// 目标用户；非管理员只能省略或填自己
    pub username: Option<String>,
    pub book_url: Option<String>,
    pub kind: Option<CacheKind>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatsQuery {
    pub username: Option<String>,
    /// 汇总全部用户；仅管理员可用
    pub all: Option<bool>,
}

/// 显式清理缓存。
///
/// 缓存不再按时间过期，清理完全由调用方驱动，因此这是唯一的回收入口。
/// 普通用户只能清自己的命名空间；`scope=all` 仅管理员可用。
pub async fn purge_cache(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<PurgeCacheRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let purged = match req.scope.unwrap_or(PurgeScope::User) {
        PurgeScope::All => {
            require_admin(&user.0)?;
            state.book_service.purge_all_cache().await?
        }
        PurgeScope::User => {
            let ns = target_ns(&user.0, req.username.as_deref())?;
            state.book_service.purge_user_cache(&ns).await?
        }
        PurgeScope::Book => {
            let ns = target_ns(&user.0, req.username.as_deref())?;
            let book_url = non_empty(req.book_url.as_deref())
                .ok_or_else(|| AppError::BadRequest("bookUrl required".to_string()))?;
            state
                .book_service
                .purge_book_cache(&ns, book_url, None)
                .await?
        }
        PurgeScope::Kind => {
            let ns = target_ns(&user.0, req.username.as_deref())?;
            let kind = req
                .kind
                .ok_or_else(|| AppError::BadRequest("kind required".to_string()))?;
            state.book_service.purge_user_cache_kind(&ns, kind).await?
        }
    };
    Ok(Json(ApiResponse::ok(serde_json::json!({
        "purged": purged,
    }))))
}

/// 各层缓存的当前占用，便于确认清理效果。
pub async fn cache_stats(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<CacheStatsQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let scope = if q.all.unwrap_or(false) {
        require_admin(&user.0)?;
        None
    } else {
        Some(target_ns(&user.0, q.username.as_deref())?)
    };
    let stats = state.book_service.cache_stats(scope.as_deref()).await?;
    Ok(Json(ApiResponse::ok(stats)))
}

/// 解析目标命名空间：默认自己；指定他人需要管理员权限。
fn target_ns(user: &AuthUser, requested: Option<&str>) -> Result<String, AppError> {
    let Some(ns) = non_empty(requested) else {
        return Ok(user.ns.clone());
    };
    if ns == user.ns {
        return Ok(ns.to_string());
    }
    if !user.is_admin {
        return Err(AppError::Forbidden(FORBIDDEN.to_string()));
    }
    if !is_valid_user_ns(ns) {
        return Err(AppError::BadRequest("非法的用户命名空间".to_string()));
    }
    Ok(ns.to_string())
}

fn require_admin(user: &AuthUser) -> Result<(), AppError> {
    if user.is_admin {
        Ok(())
    } else {
        Err(AppError::Forbidden(FORBIDDEN.to_string()))
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(is_admin: bool) -> AuthUser {
        AuthUser {
            username: "reader1".to_string(),
            ns: "reader1".to_string(),
            token: "t".to_string(),
            is_admin,
            enable_webdav: false,
            enable_ai_model: false,
        }
    }

    #[test]
    fn defaults_to_the_callers_own_namespace() {
        assert_eq!(target_ns(&user(false), None).unwrap(), "reader1");
        assert_eq!(target_ns(&user(false), Some("  ")).unwrap(), "reader1");
        assert_eq!(target_ns(&user(false), Some("reader1")).unwrap(), "reader1");
    }

    #[test]
    fn non_admin_cannot_target_another_namespace() {
        let err = target_ns(&user(false), Some("reader2")).unwrap_err();
        assert!(matches!(err, AppError::Forbidden(m) if m == FORBIDDEN));
    }

    #[test]
    fn admin_may_target_another_namespace_but_not_a_path_like_one() {
        assert_eq!(target_ns(&user(true), Some("reader2")).unwrap(), "reader2");
        let err = target_ns(&user(true), Some("../data")).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn only_admin_may_purge_everything() {
        assert!(require_admin(&user(true)).is_ok());
        assert!(require_admin(&user(false)).is_err());
    }
}
