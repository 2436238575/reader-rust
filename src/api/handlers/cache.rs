use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::Value;

use crate::api::AppState;
use crate::auth::extractor::AuthUser;
use crate::auth::{is_valid_user_ns, CurrentUser};
use crate::error::error::{ApiResponse, AppError};
use crate::service::book_service::CacheKind;

/// 清理粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PurgeScope {
    /// 全部缓存
    All,
    /// 按命名空间清理（默认自己）
    User,
    /// 单本书的缓存
    Book,
    /// 某一层缓存（正文 / 封面 / 评论…）
    Kind,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeCacheRequest {
    pub scope: Option<PurgeScope>,
    /// 目标命名空间；省略即自己
    pub username: Option<String>,
    pub book_url: Option<String>,
    pub kind: Option<CacheKind>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatsQuery {
    pub username: Option<String>,
    /// 汇总全部命名空间
    pub all: Option<bool>,
}

/// 显式清理缓存。
///
/// 缓存不再按时间过期，清理完全由调用方驱动，因此这是唯一的回收入口。
/// 单用户部署没有权限分层：登录即可清理任意范围。
pub async fn purge_cache(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<PurgeCacheRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let purged = match req.scope.unwrap_or(PurgeScope::User) {
        PurgeScope::All => state.book_service.purge_all_cache().await?,
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
        None
    } else {
        Some(target_ns(&user.0, q.username.as_deref())?)
    };
    let stats = state.book_service.cache_stats(scope.as_deref()).await?;
    Ok(Json(ApiResponse::ok(stats)))
}

/// 解析目标命名空间：默认自己；显式指定时必须合法（ns 直接拼进 storage 路径）。
fn target_ns(user: &AuthUser, requested: Option<&str>) -> Result<String, AppError> {
    let Some(ns) = non_empty(requested) else {
        return Ok(user.ns.clone());
    };
    if !is_valid_user_ns(ns) {
        return Err(AppError::BadRequest("非法的用户命名空间".to_string()));
    }
    Ok(ns.to_string())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> AuthUser {
        AuthUser {
            username: "reader1".to_string(),
            ns: "reader1".to_string(),
            token: "t".to_string(),
        }
    }

    #[test]
    fn defaults_to_the_callers_own_namespace() {
        assert_eq!(target_ns(&user(), None).unwrap(), "reader1");
        assert_eq!(target_ns(&user(), Some("  ")).unwrap(), "reader1");
        assert_eq!(target_ns(&user(), Some("reader1")).unwrap(), "reader1");
    }

    #[test]
    fn explicit_namespace_must_be_valid() {
        assert_eq!(target_ns(&user(), Some("reader2")).unwrap(), "reader2");
        let err = target_ns(&user(), Some("../data")).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }
}
