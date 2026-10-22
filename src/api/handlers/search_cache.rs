//! 缓存内全文检索：只扫已缓存章节的正文文件，不触发任何上游请求。
//!
//! 与 `/reader3/searchBook*`（向书源发起远程搜书）完全无关，这条是
//! 「在已下载的内容里找词」——目录缓存给章节表，章节缓存给正文。
//! 本地书走另一套存储（data/local_books），不在检索范围。

use crate::api::extract::QueryOrBody;
use crate::api::AppState;
use crate::auth::CurrentUser;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::Value;

use super::cache::target_ns;
use crate::error::error::{ApiResponse, AppError};

/// 搜索词长度上限：再长的词没有意义，也挡住拿超长串刷正匹配的开销。
const MAX_QUERY_CHARS: usize = 100;
/// 命中章节数上限（超出截断，`truncated: true`）。
const MAX_RESULTS: usize = 200;
const DEFAULT_MAX_RESULTS: usize = 50;

#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SearchCachedContentRequest {
    pub book_id: String,
    pub query: String,
    pub max_results: usize,
    /// 管理员代查时的目标命名空间（默认自己）
    pub username: Option<String>,
}

impl Default for SearchCachedContentRequest {
    fn default() -> Self {
        Self {
            book_id: String::new(),
            query: String::new(),
            max_results: DEFAULT_MAX_RESULTS,
            username: None,
        }
    }
}

pub async fn search_cached_content(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<SearchCachedContentRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let ns = target_ns(&user.0, req.username.as_deref())?;
    if req.book_id.trim().is_empty() {
        return Err(AppError::BadRequest("bookId required".to_string()));
    }
    // bookId 会拼进缓存目录路径：格式 + 存在 + 归属校验（与书籍级接口一致）
    super::book::resolve_book(&state, &ns, &req.book_id).await?;
    let query = req.query.trim();
    if query.is_empty() {
        return Err(AppError::BadRequest("query required".to_string()));
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(AppError::BadRequest("搜索词最长 100 个字符".to_string()));
    }
    let max_results = if req.max_results == 0 {
        DEFAULT_MAX_RESULTS
    } else {
        req.max_results.min(MAX_RESULTS)
    };
    let outcome = state
        .book_service
        .search_cached_content(&ns, &req.book_id, query, max_results)
        .await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(outcome).unwrap_or_default(),
    )))
}
