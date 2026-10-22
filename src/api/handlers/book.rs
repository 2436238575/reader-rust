use crate::api::extract::QueryOrBody;
use crate::api::handlers::image::ok_with_cover_routes;
use crate::api::handlers::multipart::read_limited_multipart_field;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::{book::Book, book_source::BookSource, search::SearchBook};
use crate::service::image_service::ImageService;
use crate::service::local_epub_book::{
    is_local_epub_origin, is_local_epub_url, LOCAL_EPUB_ORIGIN, MAX_EPUB_UPLOAD_BYTES,
};
use crate::service::local_txt_book::{
    is_local_txt_origin, is_local_txt_url, LOCAL_TXT_ORIGIN, MAX_TXT_UPLOAD_BYTES,
};
use crate::util::text::{normalize_source_url, repair_encoded_url};
use axum::body::Body;
use axum::http::header;
use axum::response::sse::{Event, KeepAlive};
use axum::response::{Response, Sse};
use axum::{
    extract::{Multipart, Query, State},
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

#[derive(Debug, Deserialize)]
pub struct ChapterListRequest {
    #[serde(rename = "bookId")]
    pub book_id: Option<String>,
    pub refresh: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct BookContentRequest {
    #[serde(rename = "chapterUrl")]
    pub chapter_url: Option<String>,
    #[serde(rename = "bookId")]
    pub book_id: Option<String>,
    pub index: Option<i32>,
    pub refresh: Option<i32>,
    /// `upstream`：只读上游基准（网络书的正文缓存 / 本地书原文），
    /// 不读编辑版、不抓上游——章节编辑 diff 的基准通道。
    pub variant: Option<String>,
}

/// 编辑版优先：有编辑版时阅读器看的是编辑版，上游缓存只做 diff 基准。
/// refresh=1 仍会重抓上游更新缓存（基准），但返回值以编辑版为准。
async fn apply_chapter_edit_overlay(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    chapter_url: &str,
    upstream: String,
) -> String {
    match state
        .chapter_edit_service
        .get_chapter_edit(user_ns, book_id, chapter_url)
        .await
    {
        Ok(Some(edited)) => edited,
        _ => upstream,
    }
}

#[derive(Debug, Deserialize)]
pub struct DeleteCacheRequest {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SaveBookProgressRequest {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
    index: Option<i32>,
    position: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct GetShelfBookRequest {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LocalEpubAssetQuery {
    #[serde(rename = "bookId")]
    book_id: String,
    path: String,
}

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

#[derive(Debug, Deserialize, Default)]
pub struct SetBookSourceRequest {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
    /// 换源目标：候选搜索结果自己的 bookId（搜索产出即登记）
    #[serde(rename = "targetBookId")]
    target_book_id: Option<String>,
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

pub async fn get_chapter_list(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ChapterListRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let do_refresh = req.refresh.unwrap_or(0) > 0;
    let resolved = resolve_book(&state, &user_ns, &book_id).await?;

    if is_local_txt_url(&resolved.book_url) {
        let chapters = state
            .local_txt_book_service
            .get_chapter_list(&user_ns, &resolved.book_url)
            .await?;
        return Ok(Json(ApiResponse::ok(
            serde_json::to_value(chapters).unwrap_or_default(),
        )));
    }
    if is_local_epub_url(&resolved.book_url) {
        let chapters = state
            .local_epub_book_service
            .get_chapter_list(&user_ns, &resolved.book_url)
            .await?;
        return Ok(Json(ApiResponse::ok(
            serde_json::to_value(chapters).unwrap_or_default(),
        )));
    }

    // 目录地址：登记表 > 书籍详情（详情缓存按 bookId，顺带回填 toc_url）> book_url
    let toc_url = match resolved.toc_url.filter(|u| !u.trim().is_empty()) {
        Some(toc_url) => toc_url,
        None => state
            .book_service
            .get_book_info(
                &user_ns,
                &book_id,
                &resolved.source,
                &resolved.book_url,
                false,
            )
            .await?
            .toc_url
            .unwrap_or_else(|| resolved.book_url.clone()),
    };
    let toc_url = repair_encoded_url(&toc_url);

    if do_refresh {
        let _ = state
            .book_service
            .delete_chapter_list_cache(&user_ns, &book_id)
            .await;
    } else if let Ok(Some(cached)) = state
        .book_service
        .load_fresh_chapter_list_cache(&user_ns, &book_id)
        .await
    {
        if !cached.is_empty() {
            return Ok(Json(ApiResponse::ok(
                serde_json::to_value(cached).unwrap_or_default(),
            )));
        }
    }

    // Get first page of chapters
    let (chapters, pagination) = state
        .book_service
        .get_chapter_list_first_page(&user_ns, &resolved.source, &toc_url)
        .await?;

    // Save first page to cache immediately
    let _ = state
        .book_service
        .save_chapter_list_cache(&user_ns, &book_id, &chapters)
        .await;

    // If there are more pages to fetch, do it in background.
    // 同一本书的补全任务并发去重：双击/刷新在首屏落盘前竞态时，
    // 两个任务会各自抓全部分页并互相覆盖，缓存里出现重复章节。
    let toc_guard = if !pagination.pending_urls.is_empty() {
        state.book_service.try_begin_toc_fill(&user_ns, &book_id)
    } else {
        None
    };
    if let Some(toc_guard) = toc_guard {
        let state_clone = state.clone();
        let user_ns_clone = user_ns.clone();
        let book_id_clone = book_id.clone();

        tokio::spawn(async move {
            // RAII：任务结束/panic 都自动释放补全标志位
            let _toc_guard = toc_guard;
            tracing::debug!("starting background chapter fetch");
            let remaining = match state_clone
                .book_service
                .fetch_remaining_chapters(pagination)
                .await
            {
                Ok(remaining) if !remaining.is_empty() => Some(remaining),
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!("background chapter fetch failed: {:?}", e);
                    None
                }
            };
            if let Some(remaining) = remaining {
                match state_clone
                    .book_service
                    .append_chapter_list_cache(&user_ns_clone, &book_id_clone, &remaining)
                    .await
                {
                    Ok(all_chapters) => {
                        tracing::debug!(
                            "background chapter fetch complete: {}",
                            all_chapters.len()
                        );
                    }
                    Err(e) => {
                        tracing::warn!("failed to append background chapters to cache: {:?}", e);
                    }
                }
            }
        });
    }

    Ok(Json(ApiResponse::ok(
        serde_json::to_value(chapters).unwrap_or_default(),
    )))
}

pub async fn get_book_content(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<BookContentRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let do_refresh = req.refresh.unwrap_or(0) > 0;
    let want_upstream = req.variant.as_deref() == Some("upstream");

    if req.chapter_url.as_deref().is_some_and(is_local_txt_url) {
        let url = req
            .chapter_url
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("chapterUrl required".to_string()))?;
        let chapter_url = if is_local_txt_url(url) && !url.contains('#') {
            let index = req.index.unwrap_or(0).max(0) as usize;
            format!(
                "{}#{}",
                repair_encoded_url(url).trim_end_matches('#'),
                index
            )
        } else {
            repair_encoded_url(url)
        };
        // 编辑版优先（本地书的 diff 基准是原文，编辑不改原文件）
        if let Some(book_id) = req.book_id.as_deref().filter(|v| !v.trim().is_empty()) {
            if !want_upstream {
                if let Ok(Some(edited)) = state
                    .chapter_edit_service
                    .get_chapter_edit(&user_ns, book_id, &chapter_url)
                    .await
                {
                    return Ok(Json(ApiResponse::ok(serde_json::Value::String(edited))));
                }
            }
        }
        let content = state
            .local_txt_book_service
            .get_content(&user_ns, &chapter_url)
            .await?;
        return Ok(Json(ApiResponse::ok(serde_json::Value::String(content))));
    }

    if req.chapter_url.as_deref().is_some_and(is_local_epub_url) {
        let url = req
            .chapter_url
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("chapterUrl required".to_string()))?;
        let chapter_url = if is_local_epub_url(url) && !url.contains('#') {
            let index = req.index.unwrap_or(0).max(0) as usize;
            format!(
                "{}#{}",
                repair_encoded_url(url).trim_end_matches('#'),
                index
            )
        } else {
            repair_encoded_url(url)
        };
        if let Some(book_id) = req.book_id.as_deref().filter(|v| !v.trim().is_empty()) {
            if !want_upstream {
                if let Ok(Some(edited)) = state
                    .chapter_edit_service
                    .get_chapter_edit(&user_ns, book_id, &chapter_url)
                    .await
                {
                    return Ok(Json(ApiResponse::ok(serde_json::Value::String(edited))));
                }
            }
        }
        let content = state
            .local_epub_book_service
            .get_content(&user_ns, &chapter_url)
            .await?;
        return Ok(Json(ApiResponse::ok(serde_json::Value::String(content))));
    }

    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let chapter_url = req
        .chapter_url
        .filter(|v| !v.trim().is_empty())
        .map(|v| repair_encoded_url(&v))
        .ok_or_else(|| AppError::BadRequest("chapterUrl required".to_string()))?;
    let resolved = resolve_book(&state, &user_ns, &book_id).await?;

    // diff 基准通道：只读上游缓存，不抓上游、不读编辑版
    if want_upstream {
        let cached = state
            .book_service
            .get_cached_content(&user_ns, &book_id, &chapter_url)
            .await?;
        return Ok(Json(ApiResponse::ok(serde_json::Value::String(
            cached.unwrap_or_default(),
        ))));
    }

    // 编辑版短路：不进抓取链路（FileCache 里的基准原样保留）
    if !do_refresh {
        if let Ok(Some(edited)) = state
            .chapter_edit_service
            .get_chapter_edit(&user_ns, &book_id, &chapter_url)
            .await
        {
            return Ok(Json(ApiResponse::ok(serde_json::Value::String(edited))));
        }
    }

    // If refresh is requested, delete this chapter's cache before fetching
    if do_refresh {
        let _ = state
            .book_service
            .delete_chapter_cache(&user_ns, &book_id, &chapter_url)
            .await;
    }

    let content = state
        .book_service
        .get_content(&user_ns, &book_id, &resolved.source, &chapter_url)
        .await?;
    // 强刷后基准已更新，阅读仍以编辑版为准
    let content =
        apply_chapter_edit_overlay(&state, &user_ns, &book_id, &chapter_url, content).await;
    Ok(Json(ApiResponse::ok(serde_json::Value::String(content))))
}

pub async fn delete_book_cache(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<DeleteCacheRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    // bookId 会拼进 toc/详情/候选清单的缓存文件路径：格式 + 存在 + 归属校验
    //（与书籍级接口一致）；`../../x` 这类输入在这里就 404，到不了文件系统
    resolve_book(&state, &user_ns, &book_id).await?;

    // 正文 + 章节列表 + 换源候选；评论（7 天 TTL 的第三方数据）与书籍详情不动
    let deleted_content = state
        .book_service
        .delete_book_cache(&user_ns, &book_id)
        .await?;
    let deleted_chapter_list = state
        .book_service
        .chapter_list_cache_exists(&user_ns, &book_id)
        .await;
    if deleted_chapter_list {
        state
            .book_service
            .delete_chapter_list_cache(&user_ns, &book_id)
            .await?;
    }
    let _ = state
        .book_service
        .delete_book_sources_cache(&user_ns, &book_id)
        .await;

    Ok(Json(ApiResponse::ok(serde_json::json!({
        "deleted": true,
        "contentCache": deleted_content,
        "chapterListCache": deleted_chapter_list
    }))))
}

pub async fn get_bookshelf(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let list = state.book_service.get_bookshelf(&user_ns).await?;
    Ok(ok_with_cover_routes(&state, serde_json::to_value(list).unwrap_or_default()).await)
}

async fn ensure_user_local_book_limit(
    state: &AppState,
    user_ns: &str,
    incoming_book_url: &str,
) -> Result<(), AppError> {
    let limit = state.config.user_local_book_limit;
    if limit == 0 {
        return Ok(());
    }
    let books = state.book_service.get_bookshelf(user_ns).await?;
    if local_book_limit_exceeded(&books, incoming_book_url, limit) {
        return Err(AppError::BadRequest(format!(
            "本地书籍数量不能超过 {limit} 本"
        )));
    }
    Ok(())
}

pub async fn upload_txt_book(
    State(state): State<AppState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let mut file_name = String::new();
    let mut bytes: Option<Vec<u8>> = None;
    let mut field_count = 0usize;
    while let Some(field) =
        crate::api::handlers::multipart::next_field_limited(&mut multipart, &mut field_count)
            .await?
    {
        let field_name = field.name().unwrap_or_default().to_string();
        if field_name != "file" {
            continue;
        }
        file_name = field.file_name().unwrap_or("book.txt").to_string();
        bytes = Some(
            read_limited_multipart_field(field, MAX_TXT_UPLOAD_BYTES, "TXT 文件不能超过 50MB")
                .await?,
        );
        break;
    }

    let bytes = bytes.ok_or_else(|| AppError::BadRequest("file required".to_string()))?;
    let book = state
        .local_txt_book_service
        .import_txt_book(&user_ns, &file_name, &bytes)
        .await?;
    if let Err(err) = ensure_user_local_book_limit(&state, &user_ns, &book.book_url).await {
        if let Err(cleanup_err) = state
            .local_txt_book_service
            .delete_book_files(&user_ns, &book.book_url)
            .await
        {
            tracing::warn!(
                "failed to clean up rejected local txt book {}: {:?}",
                book.book_url,
                cleanup_err
            );
        }
        return Err(err);
    }
    let saved = state.book_service.save_book(&user_ns, book).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(saved).unwrap_or_default(),
    )))
}

pub async fn upload_epub_book(
    State(state): State<AppState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let mut file_name = String::new();
    let mut bytes: Option<Vec<u8>> = None;
    let mut field_count = 0usize;
    while let Some(field) =
        crate::api::handlers::multipart::next_field_limited(&mut multipart, &mut field_count)
            .await?
    {
        let field_name = field.name().unwrap_or_default().to_string();
        if field_name != "file" {
            continue;
        }
        file_name = field.file_name().unwrap_or("book.epub").to_string();
        bytes = Some(
            read_limited_multipart_field(field, MAX_EPUB_UPLOAD_BYTES, "EPUB 文件不能超过 80MB")
                .await?,
        );
        break;
    }

    let bytes = bytes.ok_or_else(|| AppError::BadRequest("file required".to_string()))?;
    let book = state
        .local_epub_book_service
        .import_epub_book(&user_ns, &file_name, bytes)
        .await?;
    if let Err(err) = ensure_user_local_book_limit(&state, &user_ns, &book.book_url).await {
        if let Err(cleanup_err) = state
            .local_epub_book_service
            .delete_book_files(&user_ns, &book.book_url)
            .await
        {
            tracing::warn!(
                "failed to clean up rejected local epub book {}: {:?}",
                book.book_url,
                cleanup_err
            );
        }
        return Err(err);
    }
    let saved = state.book_service.save_book(&user_ns, book).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(saved).unwrap_or_default(),
    )))
}

/// 把前端可能存回来的「本站取图地址」还原成书源地址。
///
/// 封面在响应里被改写成了 `/reader3/image/<id>`，前端编辑/保存书籍时会把
/// 这个值原样发回来；直接落盘就等于把书源地址弄丢了，回源刷新也就没了依据。
async fn restore_cover_urls(state: &AppState, books: &mut [Book]) {
    for book in books.iter_mut() {
        let Some(id) = book
            .cover_url
            .as_deref()
            .and_then(ImageService::id_from_route)
        else {
            continue;
        };
        if let Some(record) = state.image_service.record(id).await {
            book.cover_url = Some(record.source_url);
        }
    }
}

pub async fn save_book(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(mut book): Json<Book>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    if book.book_url.trim().is_empty() {
        return Err(AppError::BadRequest("bookUrl required".to_string()));
    }

    book.book_url = repair_encoded_url(&book.book_url);
    book.origin = normalize_source_url(&book.origin);
    if let Some(toc_url) = &book.toc_url {
        book.toc_url = Some(repair_encoded_url(toc_url));
    }

    // 先登记拿 bookId（详情缓存按它键控），再按需补齐元数据
    let book_id = state
        .book_service
        .register_book(
            &user_ns,
            crate::storage::db::repo::RegisterBook {
                book_url: &book.book_url,
                book_source_id: book.book_source_id,
                name: Some(&book.name),
                author: Some(&book.author),
                toc_url: book.toc_url.as_deref(),
            },
        )
        .await?;
    book.book_id = Some(book_id.clone());

    if let Some(source_id) = book.book_source_id {
        if let Some(source) = state
            .book_source_service
            .get_by_id(&user_ns, source_id)
            .await?
        {
            // origin/originName 以书源记录为准（id 才是身份）
            book.origin = source.book_source_url.clone();
            if book.origin_name.is_none() {
                book.origin_name = Some(source.book_source_name.clone());
            }
            if book.toc_url.is_none() || book.name.trim().is_empty() {
                if let Ok(info) = state
                    .book_service
                    .get_book_info(&user_ns, &book_id, &source, &book.book_url, true)
                    .await
                {
                    merge_book(&mut book, info);
                }
            }
        }
    }

    restore_cover_urls(&state, std::slice::from_mut(&mut book)).await;
    let saved = state.book_service.save_book(&user_ns, book).await?;
    Ok(ok_with_cover_routes(&state, serde_json::to_value(saved).unwrap_or_default()).await)
}

pub async fn save_books(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    // 两种形态：裸数组（普通整架替换）或 {"books": [...], "restore": true}（WebDAV 恢复）
    let (mut books, restore) = if payload.is_array() {
        (
            serde_json::from_value::<Vec<Book>>(payload)
                .map_err(|e| AppError::BadRequest(e.to_string()))?,
            false,
        )
    } else {
        let obj = payload
            .as_object()
            .ok_or_else(|| AppError::BadRequest("invalid books payload".to_string()))?;
        let books_value = obj
            .get("books")
            .cloned()
            .ok_or_else(|| AppError::BadRequest("books required".to_string()))?;
        let books = serde_json::from_value::<Vec<Book>>(books_value)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        let restore = obj
            .get("restore")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        (books, restore)
    };

    for book in &mut books {
        if book.book_url.trim().is_empty() {
            return Err(AppError::BadRequest("bookUrl required".to_string()));
        }
        book.book_url = repair_encoded_url(&book.book_url);
        book.origin = normalize_source_url(&book.origin);
        if let Some(toc_url) = &book.toc_url {
            book.toc_url = Some(repair_encoded_url(toc_url));
        }
    }

    restore_cover_urls(&state, &mut books).await;
    let saved = state
        .book_service
        .save_books(&user_ns, books, restore)
        .await?;

    if restore {
        // 登记表已清表重建：按 id 引用的数据里，book_url 与同 id 新登记行
        // 不一致的都是恢复前别的书的残留，清掉防串书
        prune_mismatched_ai_memories(&state, &user_ns).await;
        prune_mismatched_chapter_edits(&state, &user_ns).await;
        prune_mismatched_bookmarks(&state, &user_ns).await;
    }
    Ok(ok_with_cover_routes(&state, serde_json::to_value(saved).unwrap_or_default()).await)
}

/// 恢复重建登记表后，校验 AI 记忆行的 book_url 与当前同 id 书籍一致。
async fn prune_mismatched_ai_memories(state: &AppState, user_ns: &str) {
    let Ok(rows) = state.ai_book_service.list_keys(user_ns).await else {
        return;
    };
    for (book_id, book_url) in rows {
        let mismatched = match state.book_service.book_repo().get(user_ns, &book_id).await {
            Ok(Some(row)) => row.book_url != book_url,
            // 登记表里没有这个 id：恢复的书架不含它，同样是孤儿
            Ok(None) => true,
            Err(_) => false,
        };
        if mismatched {
            let _ = state.ai_book_service.delete(user_ns, &book_id).await;
        }
    }
}

/// 章节编辑版同一手法：book_url 不一致或登记表已无此 id 的索引行连同
/// 正文文件一起删（留着会在 id 复用时把旧编辑版挂到别的书上）。
async fn prune_mismatched_chapter_edits(state: &AppState, user_ns: &str) {
    let Ok(refs) = state.chapter_edit_service.list_book_refs(user_ns).await else {
        return;
    };
    for (book_id, book_url) in refs {
        let mismatched = match state.book_service.book_repo().get(user_ns, &book_id).await {
            // 空 book_url 是列加入前的旧行：无法校验，留着不杀
            Ok(Some(row)) => !book_url.is_empty() && row.book_url != book_url,
            Ok(None) => true,
            Err(_) => false,
        };
        if mismatched {
            state
                .chapter_edit_service
                .delete_book_edits(user_ns, &book_id)
                .await;
        }
    }
}

/// 书签同一手法；只处理记了 book_url 的行（旧数据空串跳过校验）。
async fn prune_mismatched_bookmarks(state: &AppState, user_ns: &str) {
    let Ok(refs) = state.bookmark_repo.list_book_refs(user_ns).await else {
        return;
    };
    for (book_id, book_url) in refs {
        let mismatched = match state.book_service.book_repo().get(user_ns, &book_id).await {
            Ok(Some(row)) => row.book_url != book_url,
            Ok(None) => true,
            Err(_) => false,
        };
        if mismatched {
            let _ = state.bookmark_repo.delete_for_book(user_ns, &book_id).await;
        }
    }
}

pub async fn set_book_source(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<SetBookSourceRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let target_id = req
        .target_book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("targetBookId required".to_string()))?;
    if target_id == book_id {
        return Err(AppError::BadRequest("换源目标与当前书相同".to_string()));
    }

    let shelf_book = state
        .book_service
        .get_shelf_book(&user_ns, &book_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("书籍未加入书架".to_string()))?;
    let repo = state.book_service.book_repo();
    let target = repo
        .get(&user_ns, &target_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("目标书籍不存在".to_string()))?;
    // 目标是另一本在架书：合并会让两本书共享 bookId，明确拒绝
    if state
        .book_service
        .get_shelf_book(&user_ns, &target_id)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict("目标书籍已在书架".to_string()));
    }
    let new_source_id = target
        .book_source_id
        .ok_or_else(|| AppError::BadRequest("书源不存在".to_string()))?;
    let new_source = state
        .book_source_service
        .get_by_id(&user_ns, new_source_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("书源不存在".to_string()))?;

    // 换源候选清单先读出来（下面清缓存不删它，候选对下一次换源仍然有效）
    let candidates = state
        .book_service
        .load_book_sources_cache(&user_ns, &book_id)
        .await?
        .unwrap_or_default();

    // 改绑：bookId 不变，指向新地址与新书源；目标登记行是浏览残留，连带缓存删掉
    repo.rebind(&user_ns, &book_id, &target.book_url, target.book_source_id)
        .await?;
    repo.delete(&user_ns, &target_id).await?;
    let _ = state
        .book_service
        .purge_book_cache(&user_ns, &target_id)
        .await;
    state.image_service.remove_by_book_id(&target_id).await;
    // 当前书的源相关缓存（正文/目录/详情/评论）随源失效，候选清单保留
    let _ = state
        .book_service
        .purge_book_reading_cache(&user_ns, &book_id)
        .await;

    let mut updated = shelf_book.clone();
    updated.book_url = target.book_url.clone();
    updated.origin = new_source.book_source_url.clone();
    updated.origin_name = Some(new_source.book_source_name.clone());
    updated.book_source_id = new_source.book_source_id;
    updated.toc_url = None;

    if let Some(candidate) = candidates
        .into_iter()
        .find(|item| item.book_id.as_deref() == Some(target_id.as_str()))
    {
        if !candidate.name.trim().is_empty() {
            updated.name = candidate.name;
        }
        if !candidate.author.trim().is_empty() {
            updated.author = candidate.author;
        }
        updated.cover_url = candidate.cover_url.or(updated.cover_url);
        updated.intro = candidate.intro.or(updated.intro);
        updated.kind = candidate.kind.or(updated.kind);
        updated.latest_chapter_title = candidate.last_chapter.or(updated.latest_chapter_title);
    }

    match state
        .book_service
        .get_book_info(&user_ns, &book_id, &new_source, &target.book_url, true)
        .await
    {
        Ok(info) => merge_book(&mut updated, info),
        Err(err) => {
            tracing::warn!(
                "setBookSource: failed to refresh metadata for {} via {}: {:?}",
                target.book_url,
                new_source.book_source_url,
                err
            );
        }
    }

    // save_book 按 bookId 匹配原条目：进度/分组等字段随条目保留
    let saved = state.book_service.save_book(&user_ns, updated).await?;
    Ok(ok_with_cover_routes(&state, serde_json::to_value(saved).unwrap_or_default()).await)
}

#[derive(Debug, Deserialize)]
pub struct DeleteBookRequest {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteBooksRequest {
    #[serde(rename = "bookIds")]
    book_ids: Option<Vec<String>>,
}

pub async fn delete_book(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<DeleteBookRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    // 本地书文件与 AI 记忆的清理需要删前的书架条目（拿 bookUrl 判断本地类型）
    let shelf_book = state
        .book_service
        .get_shelf_book(&user_ns, &book_id)
        .await?;
    let deleted = state.book_service.delete_book(&user_ns, &book_id).await?;
    if !deleted {
        return Err(AppError::BadRequest("书架书籍不存在".to_string()));
    }
    let _ = state.ai_book_service.delete(&user_ns, &book_id).await;
    super::bookmark::delete_bookmarks_for_book(&state, &user_ns, &book_id).await?;
    state.image_service.remove_by_book_id(&book_id).await;
    // 章节编辑版（data/ 下的用户资产）随书级联删除
    state
        .chapter_edit_service
        .delete_book_edits(&user_ns, &book_id)
        .await;
    if let Some(book) = shelf_book {
        cleanup_local_book_files(&state, &user_ns, std::slice::from_ref(&book)).await;
    }
    Ok(Json(ApiResponse::ok(serde_json::json!("删除书籍成功"))))
}

pub async fn delete_books(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<DeleteBooksRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let book_ids = req.book_ids.unwrap_or_default();
    let mut shelf_books = Vec::new();
    for book_id in &book_ids {
        if let Ok(Some(book)) = state.book_service.get_shelf_book(&user_ns, book_id).await {
            shelf_books.push(book);
        }
    }
    // 级联清理只对本用户拥有的 id 执行（在架或已登记）：图片记录是按
    // book_id 全局存的，不过这道闸的话，批量参数里混进别人的合法 id
    // 会把别人的图片清掉。顺带把 id 归一为 canonical（别名与杂项缓存同口径）。
    let mut owned_ids: Vec<String> = Vec::new();
    {
        let shelf_ids: std::collections::HashSet<&str> = shelf_books
            .iter()
            .filter_map(|b| b.book_id.as_deref())
            .collect();
        for book_id in &book_ids {
            let Some(canonical) = crate::storage::db::repo::canonical_book_id(book_id) else {
                // 非法 id 在 delete_many 里是静默 no-op；这里同样跳过，绝不能把
                // 原始输入喂给会拼文件路径的级联清理（`["../.."]` 会递归删到 data/）
                continue;
            };
            if shelf_ids.contains(canonical.as_str())
                || matches!(
                    state.book_service.book_repo().get(&user_ns, &canonical).await,
                    Ok(Some(_))
                )
            {
                owned_ids.push(canonical);
            }
        }
    }
    let count = state.book_service.delete_books(&user_ns, &book_ids).await?;
    for book_id in &owned_ids {
        let _ = state.ai_book_service.delete(&user_ns, book_id).await;
        super::bookmark::delete_bookmarks_for_book(&state, &user_ns, book_id).await?;
        state.image_service.remove_by_book_id(book_id).await;
        state
            .chapter_edit_service
            .delete_book_edits(&user_ns, book_id)
            .await;
    }
    cleanup_local_book_files(&state, &user_ns, &shelf_books).await;
    Ok(Json(ApiResponse::ok(serde_json::json!({"deleted": count}))))
}

pub async fn save_book_progress(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<SaveBookProgressRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let index = req
        .index
        .ok_or_else(|| AppError::BadRequest("index required".to_string()))?;

    let shelf_book = state
        .book_service
        .get_shelf_book(&user_ns, &book_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("书籍未加入书架".to_string()))?;

    // 无变化短路：同一章节同一位置（前端已有 10s 节流 + 去重，这里挡住
    // 重试/多端重复上报），不读目录、不重写书架文件
    let position_unchanged = match req.position {
        Some(p) => shelf_book.dur_chapter_pos == Some(p),
        None => true,
    };
    let index_changed = shelf_book.dur_chapter_index != Some(index);
    if !index_changed && position_unchanged {
        return Ok(Json(ApiResponse::ok(serde_json::json!(""))));
    }

    // 章节标题/总章数只有章节切换时才需要（在线书要整份解析目录缓存，
    // 无缓存的书甚至会触发全量目录抓取——进度 ping 不能为此买单）
    let mut chapter_title: Option<String> = None;
    let mut total_chapter_num: Option<i32> = None;
    let mut latest_chapter_title: Option<String> = None;
    if index_changed {
        if is_local_txt_url(&shelf_book.book_url) {
            if let Ok(chapters) = state
                .local_txt_book_service
                .get_chapter_list(&user_ns, &shelf_book.book_url)
                .await
            {
                if index >= 0 && (index as usize) < chapters.len() {
                    chapter_title = Some(chapters[index as usize].title.clone());
                }
                total_chapter_num = Some(chapters.len() as i32);
                latest_chapter_title = chapters.last().map(|c| c.title.clone());
            }
        } else if is_local_epub_url(&shelf_book.book_url) {
            if let Ok(chapters) = state
                .local_epub_book_service
                .get_chapter_list(&user_ns, &shelf_book.book_url)
                .await
            {
                if index >= 0 && (index as usize) < chapters.len() {
                    chapter_title = Some(chapters[index as usize].title.clone());
                }
                total_chapter_num = Some(chapters.len() as i32);
                latest_chapter_title = chapters.last().map(|c| c.title.clone());
            }
        } else if let Some(source_id) = shelf_book.book_source_id {
            if let Ok(Some(source)) = state
                .book_source_service
                .get_by_id(&user_ns, source_id)
                .await
            {
                let toc_url = shelf_book
                    .toc_url
                    .clone()
                    .unwrap_or_else(|| shelf_book.book_url.clone());
                if let Ok(chapters) = state
                    .book_service
                    .get_chapter_list(&user_ns, &book_id, &source, &toc_url)
                    .await
                {
                    if index >= 0 && (index as usize) < chapters.len() {
                        chapter_title = Some(chapters[index as usize].title.clone());
                    }
                    total_chapter_num = Some(chapters.len() as i32);
                    latest_chapter_title = chapters.last().map(|c| c.title.clone());
                }
            }
        }
    }

    state
        .book_service
        .update_shelf_book(&user_ns, &book_id, |b| {
            b.dur_chapter_index = Some(index);
            b.dur_chapter_time = Some(crate::util::time::now_ts());
            if let Some(title) = chapter_title {
                b.dur_chapter_title = Some(title);
            }
            if let Some(pos) = req.position {
                b.dur_chapter_pos = Some(pos);
            }
            if let Some(total) = total_chapter_num {
                b.total_chapter_num = Some(total);
            }
            if let Some(latest) = latest_chapter_title {
                b.latest_chapter_title = Some(latest);
            }
        })
        .await?
        .ok_or_else(|| AppError::BadRequest("书籍未加入书架".to_string()))?;
    Ok(Json(ApiResponse::ok(serde_json::json!(""))))
}

pub async fn get_shelf_book(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<GetShelfBookRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let book = state
        .book_service
        .get_shelf_book(&user_ns, &book_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("书籍不存在".to_string()))?;
    Ok(ok_with_cover_routes(&state, serde_json::to_value(book).unwrap_or_default()).await)
}

pub async fn get_shelf_book_with_cache_info(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let books = state.book_service.get_bookshelf(&user_ns).await?;
    let mut result: Vec<Value> = Vec::with_capacity(books.len());
    let mut prefetch_books = Vec::new();

    for book in books {
        let mut cached_count = 0usize;
        let Some(book_id) = book.book_id.clone() else {
            // 未登记的条目兜底为 0（正常路径 saveBook 必登记）
            let mut val = serde_json::to_value(&book).unwrap_or(serde_json::json!({}));
            if let Value::Object(ref mut map) = val {
                map.insert("cachedChapterCount".to_string(), serde_json::json!(0));
            }
            result.push(val);
            continue;
        };

        if is_local_book(&book) {
            let mut val = serde_json::to_value(&book).unwrap_or(serde_json::json!({}));
            if let Value::Object(ref mut map) = val {
                // 本地书没有正文缓存目录的概念，计数恒为 0
                map.insert("cachedChapterCount".to_string(), serde_json::json!(0));
            }
            result.push(val);
            continue;
        }

        // 只探存在性 + 数缓存目录文件，不整份解析目录 JSON：
        // 逐本解析目录在书多时是每本书一次完整反序列化
        if state
            .book_service
            .chapter_list_cache_exists(&user_ns, &book_id)
            .await
        {
            cached_count = state
                .book_service
                .cached_chapter_count(&user_ns, &book_id)
                .await
                .unwrap_or(0);
        } else {
            prefetch_books.push(book.clone());
        }

        let mut val = serde_json::to_value(&book).unwrap_or(serde_json::json!({}));
        if let Value::Object(ref mut map) = val {
            map.insert(
                "cachedChapterCount".to_string(),
                serde_json::json!(cached_count),
            );
        }
        result.push(val);
    }

    if !prefetch_books.is_empty() {
        let state_clone = state.clone();
        let user_ns_clone = user_ns.clone();
        tokio::spawn(async move {
            for book in prefetch_books {
                let (Some(book_id), Some(source_id)) = (book.book_id.clone(), book.book_source_id)
                else {
                    continue;
                };
                if let Ok(Some(source)) = state_clone
                    .book_source_service
                    .get_by_id(&user_ns_clone, source_id)
                    .await
                {
                    let mut toc_url = book.toc_url.clone();
                    if toc_url.is_none() {
                        if let Ok(info) = state_clone
                            .book_service
                            .get_book_info(&user_ns_clone, &book_id, &source, &book.book_url, false)
                            .await
                        {
                            toc_url = info.toc_url.or(Some(book.book_url.clone()));
                        }
                    }
                    let toc_url = toc_url.unwrap_or_else(|| book.book_url.clone());
                    // 与 getChapterList 的后台补全共享同一把进行中守卫，
                    // 避免两条路径同时抓同一本书的目录并互相覆盖
                    if let Some(_toc_guard) = state_clone
                        .book_service
                        .try_begin_toc_fill(&user_ns_clone, &book_id)
                    {
                        let _ = state_clone
                            .book_service
                            .get_chapter_list(&user_ns_clone, &book_id, &source, &toc_url)
                            .await;
                    }
                }
            }
        });
    }

    Ok(ok_with_cover_routes(&state, serde_json::to_value(result).unwrap_or_default()).await)
}

pub async fn get_local_epub_asset(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<LocalEpubAssetQuery>,
) -> Result<Response, AppError> {
    let user_ns = user.0.ns.clone();
    let row = state
        .book_service
        .book_repo()
        .get(&user_ns, &q.book_id)
        .await?
        .ok_or_else(|| AppError::NotFound("书籍不存在".to_string()))?;
    let asset = state
        .local_epub_book_service
        .get_asset(&user_ns, &row.book_url, &q.path)
        .await?;
    let mut resp = Response::new(Body::from(asset.bytes));
    let headers = resp.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, max-age=3600"),
    );
    if let Ok(v) = header::HeaderValue::from_str(&asset.content_type) {
        headers.insert(header::CONTENT_TYPE, v);
    }
    Ok(resp)
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
    let concurrent = req.concurrent_count.unwrap_or(24).max(1) as usize;

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
    let search_size = q.search_size.unwrap_or(50).max(1) as usize;
    let concurrent = q.concurrent_count.unwrap_or(24).max(1) as usize;
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
    let search_size = q.search_size.unwrap_or(30).max(1) as usize;
    let refresh = q.refresh.unwrap_or(0) > 0;
    let concurrent = std::cmp::max(search_size * 2, 24);
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
            .load_book_sources_cache(&user_ns, &book_id)
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
            .load_book_sources_cache(&user_ns, &book_id)
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

fn merge_book(target: &mut Book, info: Book) {
    if target.name.trim().is_empty() {
        target.name = info.name;
    }
    if target.author.trim().is_empty() {
        target.author = info.author;
    }
    if target.cover_url.is_none() {
        target.cover_url = info.cover_url;
    }
    if target.toc_url.is_none() {
        target.toc_url = info.toc_url;
    }
    if target.intro.is_none() {
        target.intro = info.intro;
    }
    if target.latest_chapter_title.is_none() {
        target.latest_chapter_title = info.latest_chapter_title;
    }
    if target.word_count.is_none() {
        target.word_count = info.word_count;
    }
    if target.origin_name.is_none() {
        target.origin_name = info.origin_name;
    }
    if target.kind.is_none() {
        target.kind = info.kind;
    }
    if target.update_time.is_none() {
        target.update_time = info.update_time;
    }
}

async fn cleanup_local_book_files(state: &AppState, user_ns: &str, books: &[Book]) {
    for book in books {
        if is_local_txt_origin(&book.origin) || is_local_txt_url(&book.book_url) {
            if let Err(err) = state
                .local_txt_book_service
                .delete_book_files(user_ns, &book.book_url)
                .await
            {
                tracing::warn!(
                    "failed to delete local txt book files for {}: {:?}",
                    book.book_url,
                    err
                );
            }
        } else if is_local_epub_origin(&book.origin) || is_local_epub_url(&book.book_url) {
            if let Err(err) = state
                .local_epub_book_service
                .delete_book_files(user_ns, &book.book_url)
                .await
            {
                tracing::warn!(
                    "failed to delete local epub book files for {}: {:?}",
                    book.book_url,
                    err
                );
            }
        }
    }
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
        build_available_book_source_response, local_book_limit_exceeded,
        should_use_available_source_cache, take_available_source_cached_matches,
        take_available_source_sse_matches,
    };
    use crate::model::{book::Book, search::SearchBook};
    use std::collections::HashSet;

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
