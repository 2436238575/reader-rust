//! 书架端点：书架查询、保存/删除书籍、换源、阅读进度与本地书限额。

use super::{is_local_book, local_book_limit_exceeded};
use crate::api::extract::QueryOrBody;
use crate::api::handlers::image::ok_with_cover_routes;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::book::Book;
use crate::service::image_service::ImageService;
use crate::service::local_epub_book::{is_local_epub_origin, is_local_epub_url};
use crate::service::local_txt_book::{is_local_txt_origin, is_local_txt_url};
use crate::util::text::{normalize_source_url, repair_encoded_url};
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::Value;

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

#[derive(Debug, Deserialize, Default)]
pub struct SetBookSourceRequest {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
    /// 换源目标：候选搜索结果自己的 bookId（搜索产出即登记）
    #[serde(rename = "targetBookId")]
    target_book_id: Option<String>,
}

pub async fn get_bookshelf(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let list = state.book_service.get_bookshelf(&user_ns).await?;
    Ok(ok_with_cover_routes(&state, serde_json::to_value(list).unwrap_or_default()).await)
}

pub(super) async fn ensure_user_local_book_limit(
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
    // 书籍的 group 是位掩码：只保留当前真实存在的组的位，备份里的悬空位
    //（旧版本非法 id、已被删的组）一并剥掉，否则过滤会出幽灵分组
    let group_mask = state.book_group_service.group_mask(&user_ns).await?;
    for book in &mut books {
        if let Some(group) = book.group {
            book.group = Some(group & group_mask);
        }
    }

    restore_cover_urls(&state, &mut books).await;
    let saved = state
        .book_service
        .save_books(&user_ns, books, restore)
        .await?;

    Ok(ok_with_cover_routes(&state, serde_json::to_value(saved).unwrap_or_default()).await)
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

    // 换源候选清单先读出来（下面清缓存不删它，候选对下一次换源仍然有效）；
    // 超过 1h TTL 的旧候选不再用于回填元数据（随后 get_book_info 强刷兜底）
    let candidates = state
        .book_service
        .load_fresh_book_sources_cache(&user_ns, &book_id)
        .await?
        .unwrap_or_default();

    // 改绑：bookId 不变，指向新地址与新书源。全局唯一索引要求新 URL 只能
    // 有一行，所以先删目标登记行再改绑，两步放在同一个事务里。
    // 目标行之前预览过的正文历史会变成没有书指向的记录，仍然保留在库中。
    let mut tx = repo.begin().await?;
    repo.delete_on(&mut tx, &target_id).await?;
    repo.rebind_on(&mut tx, &book_id, &target.book_url, target.book_source_id)
        .await?;
    tx.commit().await?;
    let _ = state
        .book_service
        .purge_book_cache(&user_ns, &target_id)
        .await;
    state.image_service.remove_by_book_id(&target_id).await;
    // 当前书的目录/详情/评论缓存随源失效，候选清单与正文历史保留
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
    crate::api::handlers::bookmark::delete_bookmarks_for_book(&state, &user_ns, &book_id).await?;
    state.image_service.remove_by_book_id(&book_id).await;
    // 登记行与正文历史保留：重新加入同一本书时 book_id 不变，历史还接得上
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
                    state
                        .book_service
                        .book_repo()
                        .get(&user_ns, &canonical)
                        .await,
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
        crate::api::handlers::bookmark::delete_bookmarks_for_book(&state, &user_ns, book_id)
            .await?;
        state.image_service.remove_by_book_id(book_id).await;
        // 正文历史保留，删书只移出书架
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
