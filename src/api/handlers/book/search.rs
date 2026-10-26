//! 书籍搜索端点：单源 `searchBook` 与多源合并 `searchBookMulti`。

use super::resolve_source;
use crate::api::extract::QueryOrBody;
use crate::api::handlers::image::ok_with_cover_routes;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use axum::{extract::State, Json};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct SearchBookRequest {
    key: Option<String>,
    page: Option<i32>,
    #[serde(rename = "bookSourceId")]
    book_source_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SearchBookMultiRequest {
    key: Option<String>,
    page: Option<i32>,
    #[serde(rename = "bookSourceIds")]
    book_source_ids: Option<Vec<i64>>,
    #[serde(rename = "bookSourceGroup")]
    book_source_group: Option<String>,
}

pub async fn search_book(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<SearchBookRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let key = req
        .key
        .ok_or_else(|| AppError::BadRequest("key required".to_string()))?;
    let page = req.page.unwrap_or(1);
    let source = resolve_source(&state, &user_ns, req.book_source_id).await?;
    let books = state
        .book_service
        .search_book(&user_ns, &source, &key, page)
        .await
        .map_err(|e| {
            tracing::error!("search_book failed: {:?}", e);
            e
        })?;
    Ok(ok_with_cover_routes(&state, serde_json::to_value(books).unwrap_or_default()).await)
}

pub async fn search_book_multi(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<SearchBookMultiRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let key = req
        .key
        .ok_or_else(|| AppError::BadRequest("key required".to_string()))?;
    let page = req.page.unwrap_or(1);

    let sources = if let Some(ids) = req.book_source_ids {
        let mut out = Vec::new();
        for id in ids {
            if let Some(s) = state.book_source_service.get_by_id(&user_ns, id).await? {
                out.push(s);
            }
        }
        out
    } else {
        let mut list = state.book_source_service.list(&user_ns).await?;
        if let Some(ref group) = req.book_source_group {
            list.retain(|s| s.book_source_group.as_deref().unwrap_or("").contains(group));
        }
        list
    };

    // 与 SSE 版同一档并发上限：书源可装几百个，不设限就是几百条并发出站连接
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(24));
    let mut tasks = Vec::new();
    for source in sources {
        let svc = state.book_service.clone();
        let k = key.clone();
        let user_ns = user_ns.clone();
        let permit = sem.clone().acquire_owned().await;
        let Ok(permit) = permit else { break };
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            svc.search_book(&user_ns, &source, &k, page).await
        }));
    }
    let mut results: Vec<crate::model::search::SearchBook> = Vec::new();
    for t in tasks {
        if let Ok(Ok(list)) = t.await {
            results.extend(list);
        }
    }

    // Merge books with same name and author
    let merged = merge_search_results(results);

    Ok(ok_with_cover_routes(&state, serde_json::to_value(merged).unwrap_or_default()).await)
}

/// Merge search results from different book sources for the same book
fn merge_search_results(
    results: Vec<crate::model::search::SearchBook>,
) -> Vec<crate::model::search::SearchBook> {
    use crate::model::search::SearchBook;
    use std::collections::HashMap;

    let mut merged: HashMap<String, SearchBook> = HashMap::new();

    for book in results {
        let key = book.merge_key();

        if let Some(existing) = merged.get_mut(&key) {
            // Add this source to the existing book
            match (existing.book_source_id, book.book_source_id) {
                (Some(a), Some(b)) if a != b => {
                    let ids = existing.book_source_ids.get_or_insert_with(|| vec![a]);
                    if !ids.contains(&b) {
                        ids.push(b);
                    }
                }
                (None, Some(b)) => {
                    existing.book_source_ids = Some(vec![b]);
                }
                _ => {}
            }

            // Fill in missing fields from this source
            if existing.cover_url.is_none() && book.cover_url.is_some() {
                existing.cover_url = book.cover_url;
            }
            if existing.intro.is_none() && book.intro.is_some() {
                existing.intro = book.intro;
            }
            if existing.kind.is_none() && book.kind.is_some() {
                existing.kind = book.kind;
            }
            if existing.last_chapter.is_none() && book.last_chapter.is_some() {
                existing.last_chapter = book.last_chapter;
            }
            if existing.update_time.is_none() && book.update_time.is_some() {
                existing.update_time = book.update_time;
            }
        } else {
            merged.insert(key, book);
        }
    }

    let mut result: Vec<SearchBook> = merged.into_values().collect();
    // Sort by name for consistent ordering
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}
