//! 换源候选端点：批量 / SSE 两路可用书源探测、失效书源清单与配套筛选工具。

use crate::api::extract::QueryOrBody;
use crate::api::handlers::image::ok_with_cover_routes;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::search::SearchBook;
use axum::response::sse::{Event, KeepAlive};
use axum::response::Sse;
use axum::{
    extract::{Query, State},
    Json,
};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_stream::wrappers::ReceiverStream;

const DEFAULT_AVAILABLE_RESULT_LIMIT: usize = 20;
const MAX_AVAILABLE_RESULT_LIMIT: usize = 100;
const DEFAULT_AVAILABLE_CONCURRENT_COUNT: usize = 8;
const MAX_AVAILABLE_CONCURRENT_COUNT: usize = 20;
const AVAILABLE_SOURCE_SSE_RESULT_LIMIT: usize = 5;

#[derive(Debug, Deserialize)]
pub struct GetAvailableBookSourceRequest {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
    refresh: Option<i32>,
    #[serde(rename = "lastIndex")]
    last_index: Option<i32>,
    #[serde(rename = "resultLimit")]
    result_limit: Option<i32>,
    #[serde(rename = "concurrentCount")]
    concurrent_count: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableBookSourceResponse {
    books: Vec<SearchBook>,
    last_index: i32,
    has_more: bool,
}

pub async fn get_invalid_book_sources(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let path = state.book_service.invalid_sources_path(&user_ns);
    if !path.exists() {
        return Ok(Json(ApiResponse::ok(serde_json::json!([]))));
    }
    let data = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    let val: Value =
        serde_json::from_str(&data).map_err(|e| AppError::BadRequest(e.to_string()))?;
    if let Value::Array(_) = val {
        Ok(Json(ApiResponse::ok(val)))
    } else {
        Ok(Json(ApiResponse::ok(serde_json::json!([val]))))
    }
}

pub async fn get_available_book_source(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<GetAvailableBookSourceRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let refresh = req.refresh.unwrap_or(0) > 0;
    let paged_request =
        !should_use_available_source_cache(refresh, req.result_limit, req.last_index);
    let result_limit = if paged_request {
        effective_available_result_limit(req.result_limit)
    } else {
        usize::MAX
    };
    let concurrent_count = effective_available_concurrent_count(req.concurrent_count);

    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;

    // 在架书取书架条目（带当前书源），未在架书用登记表行的书名/作者
    let shelf_book = state
        .book_service
        .get_shelf_book(&user_ns, &book_id)
        .await?;
    let (book_name, book_author, current_source_id) = match shelf_book {
        Some(ref b) => (b.name.clone(), b.author.clone(), b.book_source_id),
        None => {
            let row = state
                .book_service
                .book_repo()
                .get(&user_ns, &book_id)
                .await?
                .ok_or_else(|| AppError::BadRequest("书籍信息错误".to_string()))?;
            (row.name, row.author, row.book_source_id)
        }
    };

    if !paged_request {
        if let Some(list) = state
            .book_service
            .load_fresh_book_sources_cache(&user_ns, &book_id)
            .await?
        {
            // 换源候选不含当前源（SSE 路径同款过滤）
            let list: Vec<SearchBook> = list
                .into_iter()
                .filter(|b| current_source_id.is_none() || b.book_source_id != current_source_id)
                .collect();
            return Ok(Json(ApiResponse::ok(
                serde_json::to_value(list).unwrap_or_default(),
            )));
        }
    }

    let sources = state.book_source_service.list(&user_ns).await?;
    if sources.is_empty() {
        if paged_request {
            return Ok(Json(ApiResponse::ok(
                serde_json::to_value(build_available_book_source_response(
                    Vec::new(),
                    req.last_index.unwrap_or(-1),
                    false,
                    req.result_limit,
                ))
                .unwrap_or_default(),
            )));
        }
        return Ok(Json(ApiResponse::ok(serde_json::json!([]))));
    }

    let mut result: Vec<SearchBook> = Vec::new();
    let mut cursor = (req.last_index.unwrap_or(-1) + 1).max(0) as usize;
    let mut last_index = req.last_index.unwrap_or(-1);

    while cursor < sources.len() {
        let batch_end = (cursor + concurrent_count).min(sources.len());
        let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();
        for (source_index, source) in sources
            .iter()
            .enumerate()
            .skip(cursor)
            .take(batch_end - cursor)
        {
            let source = source.clone();
            let svc = state.book_service.clone();
            let name = book_name.clone();
            let author = book_author.clone();
            let user_ns_value = user_ns.clone();
            tasks.push(tokio::spawn(async move {
                let res = svc.search_book(&user_ns_value, &source, &name, 1).await;
                (source_index as i32, res, name, author)
            }));
        }

        let mut batch_results = Vec::new();
        while let Some(res) = tasks.next().await {
            if let Ok((source_index, search_result, name, author)) = res {
                let matches = match search_result {
                    Ok(list) => list
                        .into_iter()
                        .filter(|b| available_source_matches_target(b, &name, &author))
                        .collect::<Vec<_>>(),
                    Err(err) => {
                        tracing::debug!(
                            "getAvailableBookSource search failed at source index {}: {:?}",
                            source_index,
                            err
                        );
                        Vec::new()
                    }
                };
                batch_results.push((source_index, matches));
            }
        }

        batch_results.sort_by_key(|(source_index, _)| *source_index);
        for (source_index, matches) in batch_results {
            if result.len() >= result_limit {
                break;
            }
            last_index = source_index;
            for book in matches {
                if result.len() >= result_limit {
                    break;
                }
                // 换源候选不含当前源（与 SSE 路径一致）
                if current_source_id.is_some() && book.book_source_id == current_source_id {
                    continue;
                }
                result.push(book);
            }
        }

        cursor = batch_end;
        if result.len() >= result_limit {
            break;
        }
    }

    let has_more = (last_index + 1).max(0) < sources.len() as i32;
    if !has_more && req.last_index.unwrap_or(-1) < 0 {
        let _ = state
            .book_service
            .save_book_sources_cache(&user_ns, &book_id, &result)
            .await;
    }

    if paged_request {
        let response =
            build_available_book_source_response(result, last_index, has_more, req.result_limit);
        return Ok(ok_with_cover_routes(&state, response).await);
    }

    Ok(ok_with_cover_routes(&state, result).await)
}

pub async fn get_available_book_source_sse(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(req): Query<GetAvailableBookSourceRequest>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let user_ns = user.0.ns.clone();
    let refresh = req.refresh.unwrap_or(0) > 0;
    let last_index_start = req.last_index.unwrap_or(-1);
    let concurrent_count = effective_available_concurrent_count(req.concurrent_count);
    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;

    // 在架书取书架条目（带当前书源），未在架书用登记表行的书名/作者
    let shelf_book = state
        .book_service
        .get_shelf_book(&user_ns, &book_id)
        .await?;
    let (book_name, book_author, current_source_id) = match shelf_book {
        Some(ref b) => (b.name.clone(), b.author.clone(), b.book_source_id),
        None => {
            let row = state
                .book_service
                .book_repo()
                .get(&user_ns, &book_id)
                .await?
                .ok_or_else(|| AppError::BadRequest("书籍信息错误".to_string()))?;
            (row.name, row.author, row.book_source_id)
        }
    };

    let (tx, rx) = mpsc::channel::<Event>(16);

    if !refresh && last_index_start < 0 {
        if let Some(cached) = state
            .book_service
            .load_fresh_book_sources_cache(&user_ns, &book_id)
            .await?
        {
            let cached = take_available_source_cached_matches(
                cached,
                current_source_id,
                AVAILABLE_SOURCE_SSE_RESULT_LIMIT,
            );
            if cached.is_empty() {
                // Ignore stale caches that only contain the current source.
            } else {
                tokio::spawn(async move {
                    let mut last_index = -1;
                    for book in cached {
                        last_index += 1;
                        let mut payload = serde_json::json!({
                            "lastIndex": last_index,
                            "hasMore": false,
                            "data": [book]
                        });
                        state.image_service.rewrite_cover_urls(&mut payload).await;
                        if tx
                            .send(Event::default().data(payload.to_string()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    let _ = tx
                        .send(
                            Event::default().event("end").data(
                                serde_json::json!({"lastIndex": last_index, "hasMore": false})
                                    .to_string(),
                            ),
                        )
                        .await;
                });
                return Ok(
                    Sse::new(ReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default())
                );
            }
        }
    }

    let sources = state.book_source_service.list(&user_ns).await?;
    let state_clone = state.clone();
    tokio::spawn(async move {
        if sources.is_empty() {
            let _ = tx
                .send(
                    Event::default().event("end").data(
                        serde_json::json!({"lastIndex": last_index_start, "hasMore": false})
                            .to_string(),
                    ),
                )
                .await;
            return;
        }

        let mut next_index = (last_index_start + 1).max(0) as usize;
        let mut last_idx = last_index_start;
        let mut emitted = 0usize;
        let mut all_results: Vec<SearchBook> = Vec::new();
        let mut seen = std::collections::HashSet::<String>::new();
        let mut tasks = JoinSet::new();

        while next_index < sources.len() || !tasks.is_empty() {
            while tasks.len() < concurrent_count
                && next_index < sources.len()
                && emitted < AVAILABLE_SOURCE_SSE_RESULT_LIMIT
            {
                let source_index = next_index;
                let source = sources[source_index].clone();
                let svc = state_clone.book_service.clone();
                let target_name = book_name.clone();
                let target_author = book_author.clone();
                let user_ns_value = user_ns.clone();
                tasks.spawn(async move {
                    let res = svc
                        .search_book(&user_ns_value, &source, &target_name, 1)
                        .await;
                    (source_index as i32, res, target_name, target_author)
                });
                next_index += 1;
            }

            if emitted >= AVAILABLE_SOURCE_SSE_RESULT_LIMIT || tasks.is_empty() {
                break;
            }

            match tasks.join_next().await {
                Some(Ok((source_index, search_result, target_name, target_author))) => {
                    last_idx = last_idx.max(source_index);
                    match search_result {
                        Ok(list) => {
                            let matches = take_available_source_sse_matches(
                                list,
                                &target_name,
                                &target_author,
                                current_source_id,
                                &mut seen,
                                AVAILABLE_SOURCE_SSE_RESULT_LIMIT - emitted,
                            );
                            for book in matches {
                                emitted += 1;
                                all_results.push(book.clone());
                                let mut payload = serde_json::json!({
                                    "lastIndex": source_index,
                                    "hasMore": next_index < sources.len() || !tasks.is_empty(),
                                    "data": [book]
                                });
                                state_clone
                                    .image_service
                                    .rewrite_cover_urls(&mut payload)
                                    .await;
                                if tx
                                    .send(Event::default().data(payload.to_string()))
                                    .await
                                    .is_err()
                                {
                                    tasks.abort_all();
                                    return;
                                }
                                if emitted >= AVAILABLE_SOURCE_SSE_RESULT_LIMIT {
                                    break;
                                }
                            }
                        }
                        Err(err) => {
                            tracing::debug!(
                                "getAvailableBookSourceSSE search failed at source index {}: {:?}",
                                source_index,
                                err
                            );
                        }
                    }
                }
                Some(Err(err)) => {
                    tracing::debug!("getAvailableBookSourceSSE task join failed: {:?}", err);
                }
                None => break,
            }
        }

        let has_more = next_index < sources.len() || !tasks.is_empty();
        if has_more {
            tasks.abort_all();
        }
        let final_last_idx = if has_more {
            last_idx.max(next_index as i32 - 1)
        } else {
            last_idx
        };

        if !has_more && last_index_start < 0 {
            let _ = state_clone
                .book_service
                .save_book_sources_cache(&user_ns, &book_id, &all_results)
                .await;
        }

        let _ = tx
            .send(Event::default().event("end").data(
                serde_json::json!({"lastIndex": final_last_idx, "hasMore": has_more}).to_string(),
            ))
            .await;
    });

    Ok(Sse::new(ReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default()))
}

fn effective_available_result_limit(result_limit: Option<i32>) -> usize {
    result_limit
        .unwrap_or(DEFAULT_AVAILABLE_RESULT_LIMIT as i32)
        .clamp(1, MAX_AVAILABLE_RESULT_LIMIT as i32) as usize
}

fn effective_available_concurrent_count(concurrent_count: Option<i32>) -> usize {
    concurrent_count
        .unwrap_or(DEFAULT_AVAILABLE_CONCURRENT_COUNT as i32)
        .clamp(1, MAX_AVAILABLE_CONCURRENT_COUNT as i32) as usize
}

/// SSE 与批量接口的并发钳制：客户端传的 concurrentCount 只当 hint，
/// 统一收敛到 1..=max（None 取默认；0/负数为 1；超大值截到 max），
/// 避免 concurrentCount=10^9 一次 spawn 上万个任务与出站连接。
pub(super) fn clamp_concurrency(value: Option<i32>, default: usize, max: usize) -> usize {
    value.unwrap_or(default as i32).clamp(1, max as i32) as usize
}

fn should_use_available_source_cache(
    refresh: bool,
    result_limit: Option<i32>,
    last_index: Option<i32>,
) -> bool {
    !refresh && result_limit.is_none() && last_index.is_none()
}

fn build_available_book_source_response(
    mut books: Vec<SearchBook>,
    last_index: i32,
    has_more: bool,
    result_limit: Option<i32>,
) -> AvailableBookSourceResponse {
    let limit = effective_available_result_limit(result_limit);
    let has_more = has_more || books.len() > limit;
    books.truncate(limit);
    AvailableBookSourceResponse {
        books,
        last_index,
        has_more,
    }
}

fn available_source_sse_result_key(book: &SearchBook) -> String {
    // 候选产出即登记，bookId 已按 bookUrl 去重；兜底 origin::bookUrl
    book.book_id
        .clone()
        .unwrap_or_else(|| format!("{}::{}", book.origin, book.book_url))
}

fn available_source_matches_target(
    book: &SearchBook,
    target_name: &str,
    target_author: &str,
) -> bool {
    normalize_available_book_name(&book.name) == normalize_available_book_name(target_name)
        && normalize_available_author(&book.author) == normalize_available_author(target_author)
}

fn normalize_available_book_name(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn normalize_available_author(value: &str) -> String {
    let compact: String = value.chars().filter(|ch| !ch.is_whitespace()).collect();
    let without_label = compact
        .strip_prefix("作者：")
        .or_else(|| compact.strip_prefix("作者:"))
        .or_else(|| compact.strip_prefix("作者"))
        .unwrap_or(&compact);
    without_label
        .trim_start_matches(['：', ':'])
        .trim()
        .to_string()
}

fn take_available_source_cached_matches(
    cached: Vec<SearchBook>,
    excluded_source_id: Option<i64>,
    limit: usize,
) -> Vec<SearchBook> {
    let mut matches = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for book in cached {
        if matches.len() >= limit {
            break;
        }
        if excluded_source_id.is_some() && book.book_source_id == excluded_source_id {
            continue;
        }
        if seen.insert(available_source_sse_result_key(&book)) {
            matches.push(book);
        }
    }
    matches
}

fn take_available_source_sse_matches(
    books: Vec<SearchBook>,
    target_name: &str,
    target_author: &str,
    excluded_source_id: Option<i64>,
    seen: &mut std::collections::HashSet<String>,
    limit: usize,
) -> Vec<SearchBook> {
    if limit == 0 {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for book in books {
        if matches.len() >= limit {
            break;
        }
        if !available_source_matches_target(&book, target_name, target_author) {
            continue;
        }
        if excluded_source_id.is_some() && book.book_source_id == excluded_source_id {
            continue;
        }
        if seen.insert(available_source_sse_result_key(&book)) {
            matches.push(book);
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::{
        build_available_book_source_response, clamp_concurrency, should_use_available_source_cache,
        take_available_source_cached_matches, take_available_source_sse_matches,
    };
    use crate::model::search::SearchBook;
    use std::collections::HashSet;

    #[test]
    fn clamp_concurrency_treats_client_value_as_hint() {
        // None → 默认
        assert_eq!(clamp_concurrency(None, 24, 32), 24);
        // 0 / 负数 → 1（SSE 循环里 tasks.len() < concurrent 依赖 ≥1）
        assert_eq!(clamp_concurrency(Some(0), 24, 32), 1);
        assert_eq!(clamp_concurrency(Some(-5), 24, 32), 1);
        // 合法值原样通过
        assert_eq!(clamp_concurrency(Some(10), 24, 32), 10);
        // 超大值截到上限：10^9 不能变成上万个并发任务
        assert_eq!(clamp_concurrency(Some(1_000_000_000), 24, 32), 32);
    }

    #[test]
    fn available_book_source_limits_result_count_and_reports_more() {
        let books: Vec<SearchBook> = (0..25)
            .map(|index| SearchBook {
                name: format!("Book {index}"),
                author: if index == 3 {
                    "作者：Author".to_string()
                } else {
                    "Author".to_string()
                },
                ..SearchBook::default()
            })
            .collect();

        let response = build_available_book_source_response(books, 11, true, Some(20));

        assert_eq!(response.books.len(), 20);
        assert_eq!(response.books[0].name, "Book 0");
        assert_eq!(response.books[19].name, "Book 19");
        assert_eq!(response.last_index, 11);
        assert!(response.has_more);
    }

    #[test]
    fn available_book_source_paged_requests_skip_complete_cache() {
        assert!(should_use_available_source_cache(false, None, None));
        assert!(!should_use_available_source_cache(true, None, None));
        assert!(!should_use_available_source_cache(false, Some(20), None));
        assert!(!should_use_available_source_cache(false, None, Some(0)));
    }

    #[test]
    fn available_book_source_cache_ignores_current_source_only_results() {
        let cached = vec![SearchBook {
            name: "深空彼岸".to_string(),
            author: "作者：辰东".to_string(),
            book_source_id: Some(7),
            book_url: "https://m.22biqu.com/biqu2986/".to_string(),
            ..SearchBook::default()
        }];

        let matches = take_available_source_cached_matches(cached, Some(7), 5);

        assert!(matches.is_empty());
    }

    #[test]
    fn available_book_source_sse_takes_matches_until_limit() {
        let books: Vec<SearchBook> = (0..8)
            .map(|index| SearchBook {
                name: if index == 0 {
                    "Other".to_string()
                } else {
                    "Book".to_string()
                },
                author: if index == 3 {
                    "作者：Author".to_string()
                } else {
                    "Author".to_string()
                },
                book_source_id: Some(if index == 2 { 99 } else { index }),
                book_url: format!("book-{index}"),
                ..SearchBook::default()
            })
            .collect();
        let mut seen = HashSet::new();

        let matches =
            take_available_source_sse_matches(books, "Book", "Author", Some(99), &mut seen, 5);

        assert_eq!(matches.len(), 5);
        assert_eq!(matches[0].book_source_id, Some(1));
        assert_eq!(matches[1].book_source_id, Some(3));
        assert_eq!(matches[4].book_source_id, Some(6));
        assert_eq!(seen.len(), 5);
    }
}
