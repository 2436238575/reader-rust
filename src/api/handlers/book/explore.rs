//! 发现（分类浏览）与书籍详情端点。

use super::{resolve_book, resolve_source};
use crate::api::extract::QueryOrBody;
use crate::api::handlers::image::ok_with_cover_routes;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::service::local_epub_book::is_local_epub_url;
use crate::service::local_txt_book::is_local_txt_url;
use axum::{extract::State, Json};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ExploreBookRequest {
    #[serde(rename = "ruleFindUrl")]
    rule_find_url: Option<String>,
    page: Option<i32>,
    #[serde(rename = "bookSourceId")]
    book_source_id: Option<i64>,
}
#[derive(Debug, Deserialize)]
pub struct BookInfoRequest {
    #[serde(rename = "bookId")]
    pub book_id: Option<String>,
    /// `>0` 时绕过书籍详情 TTL 缓存强制抓取上游
    pub refresh: Option<i32>,
}

pub async fn explore_book(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ExploreBookRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let rule_find_url = req
        .rule_find_url
        .ok_or_else(|| AppError::BadRequest("ruleFindUrl required".to_string()))?;
    let page = req.page.unwrap_or(1);
    let source = resolve_source(&state, &user_ns, req.book_source_id).await?;
    let list = state
        .book_service
        .explore_book(&user_ns, &source, &rule_find_url, page)
        .await?;
    Ok(ok_with_cover_routes(&state, serde_json::to_value(list).unwrap_or_default()).await)
}

pub async fn get_book_info(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<BookInfoRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let resolved = resolve_book(&state, &user_ns, &book_id).await?;
    if is_local_txt_url(&resolved.book_url) {
        let mut book = state
            .local_txt_book_service
            .get_book_info(&user_ns, &resolved.book_url)
            .await?;
        book.book_id = Some(book_id.clone());
        return Ok(
            ok_with_cover_routes(&state, serde_json::to_value(book).unwrap_or_default()).await,
        );
    }
    if is_local_epub_url(&resolved.book_url) {
        let mut book = state
            .local_epub_book_service
            .get_book_info(&user_ns, &resolved.book_url)
            .await?;
        book.book_id = Some(book_id.clone());
        return Ok(
            ok_with_cover_routes(&state, serde_json::to_value(book).unwrap_or_default()).await,
        );
    }
    let book = state
        .book_service
        .get_book_info(
            &user_ns,
            &book_id,
            &resolved.source,
            &resolved.book_url,
            req.refresh.unwrap_or(0) > 0,
        )
        .await?;
    Ok(ok_with_cover_routes(&state, serde_json::to_value(book).unwrap_or_default()).await)
}
