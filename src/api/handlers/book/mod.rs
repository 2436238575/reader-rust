//! 书籍相关 HTTP 端点：书架、目录、正文、搜索/发现、换源与本地书上传。
//!
//! 按功能拆分为同名目录下的子模块；本文件只保留跨子模块共享的
//! 书籍/书源解析层（`resolve_book` / `resolve_source`）与统一再导出。

use crate::api::extract::QueryOrBody;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::AppError;
use crate::model::{book::Book, book_source::BookSource};
use crate::service::local_epub_book::{is_local_epub_origin, is_local_epub_url, LOCAL_EPUB_ORIGIN};
use crate::service::local_txt_book::{is_local_txt_origin, is_local_txt_url, LOCAL_TXT_ORIGIN};
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive};
use axum::response::Sse;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use serde::Deserialize;
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

mod available_source;
mod content;
mod explore;
mod local_upload;
mod search;
mod shelf;
mod toc;

pub use available_source::*;
pub use content::*;
pub use explore::*;
pub use local_upload::*;
pub use search::*;
pub use shelf::*;
pub use toc::*;

#[derive(Debug, Deserialize)]
pub struct CacheBookRequest {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
    /// 起始章节 URL（从它开始往后缓存）；省略即从头开始
    #[serde(rename = "tocUrl")]
    toc_url: Option<String>,
    count: Option<i32>,
    refresh: Option<i32>,
    #[serde(rename = "concurrentCount")]
    concurrent_count: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct SearchBookMultiSseRequest {
    key: Option<String>,
    #[serde(rename = "bookSourceId")]
    book_source_id: Option<i64>,
    #[serde(rename = "bookSourceGroup")]
    book_source_group: Option<String>,
    #[serde(rename = "lastIndex")]
    last_index: Option<i32>,
    #[serde(rename = "searchSize")]
    search_size: Option<i32>,
    #[serde(rename = "concurrentCount")]
    concurrent_count: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct SearchBookSourceSseRequest {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
    #[serde(rename = "bookSourceGroup")]
    book_source_group: Option<String>,
    #[serde(rename = "lastIndex")]
    last_index: Option<i32>,
    #[serde(rename = "searchSize")]
    search_size: Option<i32>,
    refresh: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct BookSourceDebugRequest {
    #[serde(rename = "bookSourceId")]
    book_source_id: Option<i64>,
    keyword: Option<String>,
}

pub async fn cache_book_sse(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<CacheBookRequest>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let user_ns = user.0.ns.clone();
    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let refresh = req.refresh.unwrap_or(0) > 0;
    let concurrent = clamp_concurrency(req.concurrent_count, 24, 32);

    let book = state
        .book_service
        .get_shelf_book(&user_ns, &book_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("请先加入书架".to_string()))?;

    let source_id = book
        .book_source_id
        .ok_or_else(|| AppError::BadRequest("未配置书源".to_string()))?;
    let source = state
        .book_source_service
        .get_by_id(&user_ns, source_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("书源不存在".to_string()))?;

    // The root TOC url for the book (for fetching the full list)
    let root_toc_url = book
        .toc_url
        .clone()
        .unwrap_or_else(|| book.book_url.clone());

    // The starting chapter URL for caching (from query params)
    let start_ch_url = req.toc_url.clone();
    let cache_count = req.count.unwrap_or(0); // 0 means all

    let mut chapters = state
        .book_service
        .get_chapter_list(&user_ns, &book_id, &source, &root_toc_url)
        .await?;

    // If a starting URL is provided, narrow down the list
    if let Some(ch_url) = start_ch_url {
        if let Some(idx) = chapters.iter().position(|c| c.url == ch_url) {
            chapters = chapters.split_off(idx);
        }
    }

    // Limit count if requested
    if cache_count > 0 && cache_count < chapters.len() as i32 {
        chapters.truncate(cache_count as usize);
    }

    if chapters.is_empty() {
        return Err(AppError::BadRequest("没有找到需要缓存的章节".to_string()));
    }

    let book_id_clone = book_id.clone();
    let (tx, rx) = mpsc::channel::<Event>(32);
    let state_clone = state.clone();
    let source_clone = source.clone();
    let user_ns_clone = user_ns.clone();

    tokio::spawn(async move {
        let mut cached_count = 0usize;
        if !refresh {
            for ch in &chapters {
                if state_clone
                    .book_service
                    .is_chapter_cached(&user_ns_clone, &book_id_clone, &ch.url)
                    .await
                {
                    cached_count += 1;
                }
            }
        }
        let mut success = 0usize;
        let mut failed = 0usize;
        let _ = tx
            .send(
                Event::default().data(
                    serde_json::json!({
                        "cachedCount": cached_count,
                        "successCount": success,
                        "failedCount": failed
                    })
                    .to_string(),
                ),
            )
            .await;

        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrent));
        let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();
        for ch in chapters {
            let already_cached = !refresh
                && state_clone
                    .book_service
                    .is_chapter_cached(&user_ns_clone, &book_id_clone, &ch.url)
                    .await;
            if already_cached {
                continue;
            }
            let permit = match sem.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };
            let svc = state_clone.book_service.clone();
            let src = source_clone.clone();
            let url = ch.url.clone();
            let b_id = book_id_clone.clone();
            let refresh_flag = refresh;
            let u_ns = user_ns_clone.clone();
            tasks.push(tokio::spawn(async move {
                let _permit = permit;
                svc.cache_chapter(&u_ns, &b_id, &src, &url, refresh_flag)
                    .await
            }));
        }

        while let Some(task) = tasks.next().await {
            match task {
                Ok(Ok(_)) => {
                    success += 1;
                    cached_count += 1;
                }
                _ => {
                    failed += 1;
                }
            }
            let _ = tx
                .send(
                    Event::default().data(
                        serde_json::json!({
                            "cachedCount": cached_count,
                            "successCount": success,
                            "failedCount": failed
                        })
                        .to_string(),
                    ),
                )
                .await;
        }

        let _ = tx
            .send(
                Event::default().event("end").data(
                    serde_json::json!({
                        "cachedCount": cached_count,
                        "successCount": success,
                        "failedCount": failed
                    })
                    .to_string(),
                ),
            )
            .await;
    });

    Ok(Sse::new(ReceiverStream::new(rx).map(Ok::<_, Infallible>)).keep_alive(KeepAlive::default()))
}

pub async fn search_book_multi_sse(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<SearchBookMultiSseRequest>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let user_ns = user.0.ns.clone();
    let key = q.key.unwrap_or_default();
    let last_index = q.last_index.unwrap_or(-1);
    let search_size = q.search_size.unwrap_or(50).clamp(1, 500) as usize;
    let concurrent = clamp_concurrency(q.concurrent_count, 24, 32);
    let book_source_id = q.book_source_id;
    let book_source_group = q.book_source_group.clone().filter(|g| !g.trim().is_empty());

    // 参数校验一律在建流前完成（400）：SSE 校验失败与流内运行期错误两条通道
    // 合并为「能预检的走 HTTP 状态码」，前端少处理一种失败形态
    if key.trim().is_empty() {
        return Err(AppError::BadRequest("请输入搜索关键字".to_string()));
    }

    let (tx, rx) = mpsc::channel::<Event>(16);
    let state_clone = state.clone();

    tokio::spawn(async move {
        let sources = if let Some(id) = book_source_id {
            match state_clone
                .book_source_service
                .get_by_id(&user_ns, id)
                .await
            {
                Ok(Some(s)) => vec![s],
                _ => {
                    let _ = tx
                        .send(Event::default().event("error").data(json_err("未配置书源")))
                        .await;
                    let _ = tx
                        .send(Event::default().event("end").data(json_end(last_index)))
                        .await;
                    return;
                }
            }
        } else {
            match state_clone.book_source_service.list(&user_ns).await {
                Ok(mut list) => {
                    if let Some(ref group) = book_source_group {
                        list.retain(|s| {
                            s.book_source_group.as_deref().unwrap_or("").contains(group)
                        });
                    }
                    if list.is_empty() {
                        let _ = tx
                            .send(
                                Event::default()
                                    .event("error")
                                    .data(json_err("未配置书源或分组为空")),
                            )
                            .await;
                        let _ = tx
                            .send(Event::default().event("end").data(json_end(last_index)))
                            .await;
                        return;
                    }
                    list
                }
                _ => {
                    let _ = tx
                        .send(Event::default().event("error").data(json_err("未配置书源")))
                        .await;
                    let _ = tx
                        .send(Event::default().event("end").data(json_end(last_index)))
                        .await;
                    return;
                }
            }
        };

        let mut idx = last_index + 1;
        let mut last_idx = last_index;
        let mut result_map = std::collections::HashSet::<String>::new();
        let mut total = 0usize;
        let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();
        let mut stop_adding = false;

        while (idx as usize) < sources.len() || !tasks.is_empty() {
            // Only add new tasks if we haven't reached search_size yet
            if !stop_adding && tasks.len() < concurrent && (idx as usize) < sources.len() {
                let source = sources[idx as usize].clone();
                let svc = state_clone.book_service.clone();
                let k = key.clone();
                let cur_idx = idx;
                let user_ns_value = user_ns.clone();
                tasks.push(tokio::spawn(async move {
                    let res = svc.search_book(&user_ns_value, &source, &k, 1).await;
                    (cur_idx, source.book_source_name, res)
                }));
                idx += 1;
                continue;
            }

            if let Some(res) = tasks.next().await {
                match res {
                    Ok((cur_idx, _source_name, Ok(list))) => {
                        // 任务并发完成、顺序不定：游标只许单调前进，
                        // 否则续搜会在「已完成最大下标」与「最后完成下标」之间漏源/重复
                        last_idx = last_idx.max(cur_idx);
                        let mut batch = Vec::new();
                        for b in list {
                            let key = b.merge_key();
                            if !result_map.contains(&key) {
                                result_map.insert(key);
                                batch.push(b);
                            }
                        }
                        if !batch.is_empty() {
                            total += batch.len();
                            // SSE 路径同样收口封面：前端不接触书源地址
                            let mut payload =
                                serde_json::json!({"lastIndex": last_idx, "data": batch});
                            state_clone
                                .image_service
                                .rewrite_cover_urls(&mut payload)
                                .await;
                            if tx
                                .send(Event::default().data(payload.to_string()))
                                .await
                                .is_err()
                            {
                                // 客户端已断开：停止剩余抓取，别白白消耗出站带宽
                                break;
                            }
                        }
                        // Stop adding new tasks when search_size is reached
                        if total >= search_size {
                            stop_adding = true;
                        }
                    }
                    Ok((cur_idx, _source_name, Err(e))) => {
                        last_idx = last_idx.max(cur_idx);
                        tracing::error!("search_book error from {}: {:?}", _source_name, e);
                    }
                    Err(e) => {
                        tracing::error!("task join error: {:?}", e);
                    }
                }
            } else {
                break;
            }
        }

        let _ = tx
            .send(Event::default().event("end").data(json_end(last_idx)))
            .await;
    });

    Ok(Sse::new(ReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default()))
}

pub async fn search_book_source_sse(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<SearchBookSourceSseRequest>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let user_ns = user.0.ns.clone();
    let book_id = q.book_id.clone().unwrap_or_default();
    let last_index = q.last_index.unwrap_or(-1);
    let search_size = q.search_size.unwrap_or(30).clamp(1, 500) as usize;
    let refresh = q.refresh.unwrap_or(0) > 0;
    // 并发上限与 search_size 解耦：换源候选的并发按结果预算放大（×2、
    // 至少 24），但整体仍钳在 32 内
    let concurrent = clamp_concurrency(Some((search_size * 2) as i32), 24, 32);
    let book_source_group = q.book_source_group.clone().filter(|g| !g.trim().is_empty());

    // 参数与前置状态校验一律在建流前完成（400）：流内只余运行期错误
    if book_id.trim().is_empty() {
        return Err(AppError::BadRequest("bookId required".to_string()));
    }
    let book = match state.book_service.get_shelf_book(&user_ns, &book_id).await {
        Ok(Some(b)) => b,
        _ => return Err(AppError::BadRequest("书籍未加入书架".to_string())),
    };
    let sources = match state.book_source_service.list(&user_ns).await {
        Ok(mut list) => {
            if let Some(ref group) = book_source_group {
                list.retain(|s| s.book_source_group.as_deref().unwrap_or("").contains(group));
            }
            if list.is_empty() {
                return Err(AppError::BadRequest("未配置书源或分组为空".to_string()));
            }
            list
        }
        _ => return Err(AppError::BadRequest("未配置书源".to_string())),
    };

    let (tx, rx) = mpsc::channel::<Event>(16);
    let state_clone = state.clone();

    tokio::spawn(async move {
        let mut idx = last_index + 1;
        let mut last_idx = last_index;
        let mut total = 0usize;
        let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();
        let mut all_results: Vec<crate::model::search::SearchBook> = Vec::new();

        while (idx as usize) < sources.len() || !tasks.is_empty() {
            while tasks.len() < concurrent && (idx as usize) < sources.len() {
                let source = sources[idx as usize].clone();
                let svc = state_clone.book_service.clone();
                let target_name = book.name.clone();
                let target_author = book.author.clone();
                let cur_idx = idx;
                let user_ns_value = user_ns.clone();
                tasks.push(tokio::spawn(async move {
                    let res = svc
                        .search_book(&user_ns_value, &source, &target_name, 1)
                        .await;
                    (cur_idx, res, target_name, target_author)
                }));
                last_idx = idx;
                idx += 1;
            }

            if let Some(res) = tasks.next().await {
                if let Ok((cur_idx, Ok(list), target_name, target_author)) = res {
                    let mut batch = Vec::new();
                    for b in list {
                        if b.name == target_name && b.author == target_author {
                            batch.push(b);
                        }
                    }
                    if !batch.is_empty() {
                        total += batch.len();
                        all_results.extend(batch.clone());
                        let mut payload = serde_json::json!({"lastIndex": cur_idx, "data": batch});
                        state_clone
                            .image_service
                            .rewrite_cover_urls(&mut payload)
                            .await;
                        if tx
                            .send(Event::default().data(payload.to_string()))
                            .await
                            .is_err()
                        {
                            // 客户端已断开：停止剩余抓取，别白白消耗出站带宽
                            break;
                        }
                    }
                    if total >= search_size {
                        break;
                    }
                }
            } else {
                break;
            }
        }

        if refresh || !all_results.is_empty() {
            let _ = state_clone
                .book_service
                .save_book_sources_cache(&user_ns, &book_id, &all_results)
                .await;
        }
        let _ = tx
            .send(Event::default().event("end").data(json_end(last_idx)))
            .await;
    });

    Ok(Sse::new(ReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default()))
}

pub async fn book_source_debug_sse(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<BookSourceDebugRequest>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let user_ns = user.0.ns.clone();
    let book_source_id = q.book_source_id;
    let keyword = q.keyword.unwrap_or_default();

    // 参数与书源存在性校验在建流前完成（400），流内只余运行期错误
    let Some(source_id) = book_source_id else {
        return Err(AppError::BadRequest("未配置书源".to_string()));
    };
    let source = match state
        .book_source_service
        .get_by_id(&user_ns, source_id)
        .await
    {
        Ok(Some(s)) => s,
        _ => return Err(AppError::BadRequest("未配置书源".to_string())),
    };

    let (tx, rx) = mpsc::channel::<Event>(16);
    let state_clone = state.clone();

    tokio::spawn(async move {
        let keyword = if keyword.trim().is_empty() {
            source
                .rule_search
                .as_ref()
                .and_then(|rule| rule.check_key_word.clone())
                .unwrap_or_default()
        } else {
            keyword.clone()
        };
        if keyword.trim().is_empty() {
            let _ = tx
                .send(
                    Event::default()
                        .event("error")
                        .data(json_err("请输入搜索关键词")),
                )
                .await;
            let _ = tx
                .send(Event::default().event("end").data(json_end(0)))
                .await;
            return;
        }
        let _ = tx
            .send(Event::default().data(json_msg("start search")))
            .await;
        match state_clone
            .book_service
            .search_book(&user_ns, &source, &keyword, 1)
            .await
        {
            Ok(list) => {
                let msg = format!("found {} items", list.len());
                let _ = tx.send(Event::default().data(json_msg(&msg))).await;
                let payload = serde_json::json!({"data": list});
                let _ = tx.send(Event::default().data(payload.to_string())).await;
            }
            Err(e) => {
                let _ = tx
                    .send(
                        Event::default()
                            .event("error")
                            .data(json_err(&e.to_string())),
                    )
                    .await;
            }
        }
        let _ = tx
            .send(Event::default().event("end").data(json_end(0)))
            .await;
    });

    Ok(Sse::new(ReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default()))
}

fn json_err(msg: &str) -> String {
    serde_json::json!({"errorMsg": msg}).to_string()
}

fn json_end(last_index: i32) -> String {
    serde_json::json!({"lastIndex": last_index}).to_string()
}

fn json_msg(msg: &str) -> String {
    serde_json::json!({"msg": msg}).to_string()
}

/// 书源级接口（搜索/发现/调试）的入口：bookSourceId 直查。
pub(crate) async fn resolve_source(
    state: &AppState,
    user_ns: &str,
    book_source_id: Option<i64>,
) -> Result<BookSource, AppError> {
    let id =
        book_source_id.ok_or_else(|| AppError::BadRequest("bookSourceId required".to_string()))?;
    state
        .book_source_service
        .get_by_id(user_ns, id)
        .await?
        .ok_or_else(|| AppError::NotFound("bookSource not found".to_string()))
}

/// 一本书的完整解析结果：登记表行 + 绑定书源。
pub(crate) struct ResolvedBook {
    pub book_url: String,
    pub toc_url: Option<String>,
    pub source: BookSource,
}

/// 书籍级接口的统一入口：bookId → 登记表 → 书源。
///
/// 本地书（source_id 为 NULL）按 book_url 前缀构造伪书源；绑定的书源被删时
/// 报「书源不存在」（与旧版按 URL 找不到书源同语义）。
pub(crate) async fn resolve_book(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
) -> Result<ResolvedBook, AppError> {
    let row = state
        .book_service
        .book_repo()
        .get(user_ns, book_id)
        .await?
        .ok_or_else(|| AppError::NotFound("书籍不存在".to_string()))?;
    let source = if is_local_txt_url(&row.book_url) {
        BookSource {
            book_source_name: "本地 TXT".to_string(),
            book_source_url: LOCAL_TXT_ORIGIN.to_string(),
            ..BookSource::default()
        }
    } else if is_local_epub_url(&row.book_url) {
        BookSource {
            book_source_name: "本地 EPUB".to_string(),
            book_source_url: LOCAL_EPUB_ORIGIN.to_string(),
            ..BookSource::default()
        }
    } else {
        let source_id = row
            .book_source_id
            .ok_or_else(|| AppError::NotFound("书源不存在".to_string()))?;
        state
            .book_source_service
            .get_by_id(user_ns, source_id)
            .await?
            .ok_or_else(|| AppError::NotFound("书源不存在".to_string()))?
    };
    Ok(ResolvedBook {
        book_url: row.book_url.clone(),
        toc_url: row.toc_url.clone(),
        source,
    })
}

fn is_local_book(book: &Book) -> bool {
    is_local_txt_origin(&book.origin)
        || is_local_txt_url(&book.book_url)
        || is_local_epub_origin(&book.origin)
        || is_local_epub_url(&book.book_url)
}

fn local_book_limit_exceeded(books: &[Book], incoming_book_url: &str, limit: u32) -> bool {
    if limit == 0 {
        return false;
    }
    if books
        .iter()
        .any(|book| is_local_book(book) && book.book_url == incoming_book_url)
    {
        return false;
    }
    books.iter().filter(|book| is_local_book(book)).count() >= limit as usize
}

#[cfg(test)]
mod tests {
    use super::local_book_limit_exceeded;
    use crate::model::book::Book;

    #[test]
    fn local_book_limit_counts_txt_and_epub_but_allows_existing_book() {
        let books = vec![
            Book {
                origin: "local-txt".to_string(),
                book_url: "local-txt:abc".to_string(),
                ..Book::default()
            },
            Book {
                origin: "local-epub".to_string(),
                book_url: "local-epub:def".to_string(),
                ..Book::default()
            },
            Book {
                origin: "https://source.test".to_string(),
                book_url: "https://source.test/book/1".to_string(),
                ..Book::default()
            },
        ];

        assert!(!local_book_limit_exceeded(&books, "local-epub:new", 0));
        assert!(local_book_limit_exceeded(&books, "local-epub:new", 2));
        assert!(!local_book_limit_exceeded(&books, "local-epub:def", 2));
        assert!(!local_book_limit_exceeded(&books, "local-epub:new", 3));
    }
}
