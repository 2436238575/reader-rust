//! `/v1/me/shelf` 写路径：入架（PUT，幂等）、进度与状态（PATCH merge-patch）、
//! 移出书架（DELETE，不删登记行与正文历史）。
//!
//! 本批的范围（book-fields.md 的元数据迁移未做）：
//! - PATCH 只收 `progress`；`groupIds` / `pinned` 未实现（groups/pinned 的
//!   存储与迁移随后续批次），传入即 422 `UNKNOWN_FIELD`；
//! - `PATCH /v1/books/{id}` 只收 `name` / `author`（登记表列）；intro/kind 等
//!   元数据的存储还在 `shelf_books`，等元数据迁移后扩展。
//!
//! 进度语义（契约 §8.2）：`offset` 是 0..1 比例（存万分比）；不传时间，
//! `lastReadAt` 由服务端写入——这是唯一可信的时钟。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use crate::model::book::Book;
use crate::storage::db::repo::RegisterBook;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 「最近阅读时间」刷新节流：窗口内的重复保存不改动时间戳。
const PROGRESS_TIME_THROTTLE_SECS: i64 = 60;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/shelf", get(list_shelf))
        .route(
            "/v1/me/shelf/{bookId}",
            get(get_shelf_item)
                .put(add_to_shelf)
                .patch(patch_shelf_book)
                .delete(remove_from_shelf),
        )
        .route("/v1/me/shelf:batchDelete", post(batch_remove_from_shelf))
        .route("/v1/books/{bookId}", get(get_book).patch(patch_book))
}

fn book_not_found() -> Problem {
    Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
}

fn book_not_in_shelf() -> Problem {
    Problem::new(ErrorCode::BookNotInShelf).detail("这本书还没有加入书架")
}

/// 入架请求体：全部字段可选（`{}` 表示全部缺省），**但请求体本身必填**——
/// 无 body / 无 `Content-Type` 会 415/400，无字段的调用方也至少传 `{}`。
/// 调用方（搜索/发现结果）已有展示字段时带上，服务端跳过回源详情——
/// 发现页直接入架、源已挂时，不让「加入书架」干等最坏 3×10s 的详情重试。
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct ShelfPutBody {
    pub name: Option<String>,
    pub author: Option<String>,
    /// 封面：本站取图地址（`/reader3/image/<id>`）会还原成上游地址落库
    pub cover_url: Option<String>,
    pub intro: Option<String>,
    pub kind: Option<String>,
}

/// 入架（幂等）：已登记的书籍确保在架。201 首次 / 200 已在架。
///
/// 元数据优先用请求体里的展示字段；没带时打书籍详情（1h 缓存，详情页刚
/// 看过时命中）填充，本地书走本地详情。详情拿不到（源挂了/离线）不阻塞
/// 入架：回退登记行信息，条目先立起来。
#[utoipa::path(
    put,
    path = "/v1/me/shelf/{bookId}",
    tag = "me",
    operation_id = "addToShelf",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    request_body = ShelfPutBody,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 201, description = "已入架", body = ShelfWriteResult),
        (status = 200, description = "已在架（幂等重复调用，不重置进度）", body = ShelfWriteResult),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "请求体含未知字段", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn add_to_shelf(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    V1Json(body): V1Json<ShelfPutBody>,
) -> Result<(StatusCode, Response), Problem> {
    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => book_not_found(),
            other => Problem::from(other),
        })?;
    let already = state
        .book_service
        .get_shelf_book(&user.ns, &book_id)
        .await?;

    // 已在架：幂等返回，不重置进度，也不再打详情（可能回源）
    if let Some(entry) = already {
        return Ok((
            StatusCode::OK,
            Json(ShelfWriteResult {
                book_id: entry.book_id.unwrap_or(book_id),
            })
            .into_response(),
        ));
    }

    let has_display_fields = body.name.is_some()
        || body.author.is_some()
        || body.cover_url.is_some()
        || body.intro.is_some()
        || body.kind.is_some();

    // 元数据：本地书走本地详情；网络书在调用方带展示字段时跳过回源，
    // 用登记行（bookUrl/origin/name/author）+ 传入字段拼条目；没带才打
    // 书籍详情（可能回源）。详情拿不到不阻塞入架：回退登记行信息
    let mut book = if crate::service::local_txt_book::is_local_txt_url(&resolved.book_url) {
        state
            .local_txt_book_service
            .get_book_info(&user.ns, &resolved.book_url)
            .await
            .map_err(Problem::from)?
    } else if crate::service::local_epub_book::is_local_epub_url(&resolved.book_url) {
        state
            .local_epub_book_service
            .get_book_info(&user.ns, &resolved.book_url)
            .await
            .map_err(Problem::from)?
    } else if has_display_fields {
        Book {
            book_url: resolved.book_url.clone(),
            book_source_id: resolved.source.book_source_id,
            origin: resolved.source.book_source_url.clone(),
            origin_name: Some(resolved.source.book_source_name.clone()),
            name: String::new(),
            author: String::new(),
            ..Book::default()
        }
    } else {
        match state
            .book_service
            .get_book_info(
                &user.ns,
                &book_id,
                &resolved.source,
                &resolved.book_url,
                false,
            )
            .await
        {
            Ok(info) => info,
            Err(error) => {
                tracing::warn!(%error, book_id, "入架时详情获取失败，用登记行信息落架");
                Book {
                    book_url: resolved.book_url.clone(),
                    book_source_id: resolved.source.book_source_id,
                    origin: resolved.source.book_source_url.clone(),
                    origin_name: Some(resolved.source.book_source_name.clone()),
                    name: String::new(),
                    author: String::new(),
                    ..Book::default()
                }
            }
        }
    };

    // 传入字段覆盖（空串不覆盖：登记行里可能有真名，与 touch_book_fields
    // 的 CASE WHEN 保护同思路）
    if let Some(name) = body
        .name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        book.name = name.to_string();
    }
    if let Some(author) = body
        .author
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        book.author = author.to_string();
    }
    if let Some(cover) = body
        .cover_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        book.cover_url = Some(restore_cover_url(&state, cover).await);
    }
    if let Some(intro) = &body.intro {
        book.intro = Some(intro.clone());
    }
    if let Some(kind) = &body.kind {
        book.kind = Some(kind.clone());
    }

    book.book_id = Some(book_id.clone());
    let saved = state.book_service.save_book(&user.ns, book).await?;

    Ok((
        StatusCode::CREATED,
        Json(ShelfWriteResult {
            book_id: saved.book_id.unwrap_or(book_id),
        })
        .into_response(),
    ))
}

/// 前端传回的本站取图地址还原成上游地址落库（与 `/reader3/saveBook` 的
/// restore_cover_urls 同语义）——直接存路由串，图片回源刷新就没了依据。
async fn restore_cover_url(state: &AppState, url: &str) -> String {
    use crate::service::image_service::ImageService;
    if let Some(id) = ImageService::id_from_route(url) {
        if let Some(record) = state.image_service.record(id).await {
            return record.source_url;
        }
    }
    url.to_string()
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShelfWriteResult {
    pub book_id: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShelfPatch {
    pub progress: Option<ProgressPatch>,
    /// 分组位掩码单值（2 的幂；0 = 移出全部分组）。对应 /reader3 的
    /// saveBookGroupId；组本身的管理走 /v1/me/groups
    pub group: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgressPatch {
    pub chapter_index: i32,
    pub chapter_title: Option<String>,
    /// 0..1 的章内比例（服务端存万分比）
    pub offset: Option<f64>,
}

/// 进度与书架状态（merge-patch，只写出现的字段）。
///
/// 未提供 `chapterTitle` 且章节切换时，标题从章节登记表解析；
/// `lastReadAt` 服务端写——客户端时间不参与比较（契约 §5.2）。
/// `group` 与 `progress` 相互独立，可只传其一。
#[utoipa::path(
    patch,
    path = "/v1/me/shelf/{bookId}",
    tag = "me",
    operation_id = "patchShelfBook",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    request_body = ShelfPatch,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已更新"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "书籍未加入书架", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "未知字段或取值非法", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn patch_shelf_book(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    V1Json(patch): V1Json<ShelfPatch>,
) -> Result<StatusCode, Problem> {
    if patch.progress.is_none() && patch.group.is_none() {
        // merge-patch 空体：合法但无事可做
        return Ok(StatusCode::NO_CONTENT);
    }
    // 分组字段：非 0 必须是已存在的合法幂位（0 = 移出全部分组）
    if let Some(group) = patch.group.filter(|value| *value != 0) {
        crate::service::book_group_service::validate_group_id(group).map_err(Problem::from)?;
        if !state
            .book_group_service
            .group_exists(&user.ns, group)
            .await
            .map_err(Problem::from)?
        {
            return Err(Problem::new(ErrorCode::ValidationFailed)
                .detail("分组不存在")
                .errors(vec![crate::api::problem::FieldIssue {
                    pointer: "/group".to_string(),
                    code: "NOT_FOUND".to_string(),
                    detail: None,
                }]));
        }
    }
    let progress = match patch.progress {
        Some(progress) => progress,
        None => {
            // 只改分组：不动进度与最近阅读时间
            let updated = state
                .book_service
                .update_shelf_book(&user.ns, &book_id, |book| {
                    if let Some(group) = patch.group {
                        book.group = Some(group);
                    }
                })
                .await
                .map_err(Problem::from)?;
            if updated.is_none() {
                return Err(book_not_in_shelf());
            }
            return Ok(StatusCode::NO_CONTENT);
        }
    };
    // offset 越界是字段值非法（契约 422 VALIDATION_FAILED，带 pointer），
    // 不做静默 clamp——静默改写客户端给的值会掩盖调用方 bug
    let offset = match progress.offset {
        Some(offset) if !(0.0..=1.0).contains(&offset) => {
            return Err(Problem::new(ErrorCode::ValidationFailed)
                .detail("progress.offset 必须在 0..1 之间")
                .errors(vec![crate::api::problem::FieldIssue {
                    pointer: "/progress/offset".to_string(),
                    code: "OUT_OF_RANGE".to_string(),
                    detail: Some("必须在 0..1 之间".to_string()),
                }]));
        }
        other => other,
    };
    let offset = offset.map(|value| (value * 10_000.0).round() as i32);
    // 章节标题缺省时从登记表解析（与目录显示一致，覆盖层后续在读取出口叠加）；
    // 只有确实切了章才解析（原地保存不改标题）
    let entry = state
        .book_service
        .get_shelf_book(&user.ns, &book_id)
        .await?
        .ok_or_else(book_not_in_shelf)?;
    let index_changed = entry
        .dur_chapter_index
        .is_none_or(|current| current != progress.chapter_index);
    let title = match &progress.chapter_title {
        Some(title) => Some(title.clone()),
        None => {
            if index_changed {
                state
                    .book_service
                    .resolve_chapter_by_index(&book_id, progress.chapter_index)
                    .await
                    .map_err(Problem::from)?
                    .map(|chapter| chapter.title)
            } else {
                None
            }
        }
    };

    let updated = state
        .book_service
        .update_shelf_book(&user.ns, &book_id, |book| {
            book.dur_chapter_index = Some(progress.chapter_index);
            if let Some(title) = title {
                book.dur_chapter_title = Some(title);
            }
            if let Some(offset) = offset {
                book.dur_chapter_pos = Some(offset);
            }
            // 最近阅读时间：每次进度保存都刷新（旧 saveBookProgress 口径），
            // 60s 节流避免高频 ping 的无谓写入；语义是「最近阅读」，
            // 不绑在换章上——同一章读两小时也不能沉下去
            let now = crate::util::time::now_ts();
            let throttled = book
                .dur_chapter_time
                .is_some_and(|last| now - last < PROGRESS_TIME_THROTTLE_SECS);
            if !throttled {
                book.dur_chapter_time = Some(now);
            }
            if let Some(group) = patch.group {
                book.group = Some(group);
            }
        })
        .await
        .map_err(Problem::from)?;
    if updated.is_none() {
        return Err(book_not_in_shelf());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// 移出书架（幂等）：登记行、书签与正文历史保留。书籍不存在同样 204。
#[utoipa::path(
    delete,
    path = "/v1/me/shelf/{bookId}",
    tag = "me",
    operation_id = "removeFromShelf",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已移出（幂等）"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn remove_from_shelf(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
) -> Result<StatusCode, Problem> {
    crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => book_not_found(),
            other => Problem::from(other),
        })?;
    state
        .book_service
        .delete_book(&user.ns, &book_id)
        .await
        .map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 批量移出上限：全选删除的现实规模是几百本，1000 留足余量。
const MAX_SHELF_BATCH_DELETE: usize = 1000;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShelfBatchDeleteRequest {
    pub book_ids: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShelfBatchDeleteResult {
    pub deleted: u32,
}

/// 批量移出（幂等）：与单个 DELETE 同语义——只摘书架行，登记行、书签与
/// 正文历史保留；不存在的 id 忽略。逐本调 `delete_book`（各自清缓存），
/// 不做整体事务：批量移出不需要「要么全删要么全留」。
#[utoipa::path(
    post,
    path = "/v1/me/shelf:batchDelete",
    tag = "me",
    operation_id = "batchRemoveFromShelf",
    request_body = ShelfBatchDeleteRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "删除条数", body = ShelfBatchDeleteResult),
        (status = 400, description = "book_ids 为空或超上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn batch_remove_from_shelf(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<ShelfBatchDeleteRequest>,
) -> Result<Json<ShelfBatchDeleteResult>, Problem> {
    if request.book_ids.is_empty() || request.book_ids.len() > MAX_SHELF_BATCH_DELETE {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail(format!(
            "book_ids 需要提供 1 到 {MAX_SHELF_BATCH_DELETE} 个"
        )));
    }
    let mut deleted = 0u32;
    for book_id in &request.book_ids {
        // 非法 id 视同不存在（与单个 DELETE 的幂等口径一致），不送进文件路径
        if crate::storage::db::repo::parse_book_id(book_id).is_none() {
            continue;
        }
        let removed = state
            .book_service
            .delete_book(&user.ns, book_id)
            .await
            .map_err(Problem::from)?;
        if removed {
            deleted += 1;
        }
    }
    Ok(Json(ShelfBatchDeleteResult { deleted }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BookPatch {
    pub name: Option<String>,
    pub author: Option<String>,
}

/// 改书名/作者（登记表列；merge-patch，只写出现的字段）。
///
/// intro/kind/wordCount/cover 的存储还在 `shelf_books`（元数据迁移未做），
/// 本端点暂不收——传入即 422 `UNKNOWN_FIELD`。
#[utoipa::path(
    patch,
    path = "/v1/books/{bookId}",
    tag = "books",
    operation_id = "patchBook",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    request_body = BookPatch,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已更新"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "未知字段（本端点只收 name/author）", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn patch_book(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    V1Json(patch): V1Json<BookPatch>,
) -> Result<StatusCode, Problem> {
    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => book_not_found(),
            other => Problem::from(other),
        })?;
    state
        .book_service
        .book_repo()
        .register(
            &user.ns,
            RegisterBook {
                book_url: &resolved.book_url,
                book_source_id: resolved.source.book_source_id,
                name: patch.name.as_deref(),
                author: patch.author.as_deref(),
                toc_url: resolved.toc_url.as_deref(),
            },
            None,
        )
        .await
        .map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 书籍详情：详情规则求值（1h 缓存）+ 章节登记表的统计切片。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}",
    tag = "books",
    operation_id = "getBook",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "书籍详情", body = BookDetail),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_book(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
) -> Result<axum::Json<BookDetail>, Problem> {
    use crate::service::local_epub_book::is_local_epub_url;
    use crate::service::local_txt_book::is_local_txt_url;

    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => book_not_found(),
            other => Problem::from(other),
        })?;
    let is_local = is_local_txt_url(&resolved.book_url) || is_local_epub_url(&resolved.book_url);
    // 详情拿不到（源挂了/离线）不阻塞读取：回退登记行信息并标 stale，
    // 详情页永远能打开（比 /reader3 的 502 行为更好）。本地书没有上游，
    // 走本地详情（目录统计/kind/EPUB 封面都由它给出），恒不 stale。
    let (info, stale) = if is_local_txt_url(&resolved.book_url) {
        (
            state
                .local_txt_book_service
                .get_book_info(&user.ns, &resolved.book_url)
                .await
                .map_err(Problem::from)?,
            false,
        )
    } else if is_local_epub_url(&resolved.book_url) {
        (
            state
                .local_epub_book_service
                .get_book_info(&user.ns, &resolved.book_url)
                .await
                .map_err(Problem::from)?,
            false,
        )
    } else {
        match state
            .book_service
            .get_book_info(
                &user.ns,
                &book_id,
                &resolved.source,
                &resolved.book_url,
                false,
            )
            .await
        {
            Ok(info) => (info, false),
            Err(error) => {
                tracing::warn!(%error, book_id, "书籍详情获取失败，回退登记行信息");
                let row = state
                    .book_service
                    .book_repo()
                    .get(&user.ns, &book_id)
                    .await?
                    .ok_or_else(book_not_found)?;
                (
                    crate::model::book::Book {
                        book_url: row.book_url,
                        book_source_id: row.book_source_id,
                        name: row.name,
                        author: row.author,
                        ..crate::model::book::Book::default()
                    },
                    true,
                )
            }
        }
    };
    // 封面出口：http(s) 进图片管道只给 id（签名的上游直出地址会过期，
    // 不能给客户端）；本地 EPUB 封面是站内资源地址（/reader3/localEpubAsset），
    // 进不了只收 http(s) 的图片管道，原样给 coverUrl
    let (cover_image_id, cover_url) = split_cover(info.cover_url.as_deref());
    // 本地书的目录不在章节登记表里，统计切片取自本地索引（Book 快照）
    let chapters = if is_local {
        Vec::new()
    } else {
        state
            .book_service
            .load_chapter_list_cache(&user.ns, &book_id)
            .await
            .map_err(Problem::from)?
            .unwrap_or_default()
    };
    let total_chapters = if is_local {
        info.total_chapter_num
            .and_then(|count| u32::try_from(count).ok())
            .filter(|count| *count > 0)
    } else {
        (!chapters.is_empty()).then_some(chapters.len() as u32)
    };
    let last_chapter = if is_local {
        info.latest_chapter_title.as_ref().map(|title| LastChapter {
            index: info.total_chapter_num.unwrap_or(1).saturating_sub(1),
            title: title.clone(),
        })
    } else {
        chapters.last().map(|chapter| LastChapter {
            index: chapter.index,
            title: chapter.title.clone(),
        })
    };

    Ok(axum::Json(BookDetail {
        id: book_id.clone(),
        name: info.name,
        author: Some(info.author).filter(|value| !value.trim().is_empty()),
        source_id: info.book_source_id.map(|id| format!("src_{id}")),
        book_url: resolved.book_url.clone(),
        intro: info.intro,
        kind: info.kind,
        cover_image_id,
        cover_url,
        total_chapters,
        last_chapter,
        stale,
    }))
}

/// 封面出口的唯一拆分点：
/// - 已是本站取图地址（`/reader3/image/<id>`）：取回 id；
/// - http(s) 上游地址：图片管道 id（`md5(去查询串地址)`），同一张图恒定；
/// - 其余站内资源地址（本地 EPUB 的 `/reader3/localEpubAsset?...`）：管道不收，
///   原样走 `coverUrl`，客户端照常直接渲染；
/// - 空串：无封面（md5("") 会指向一张不存在的图）。
fn split_cover(cover_url: Option<&str>) -> (Option<String>, Option<String>) {
    use crate::service::image_service::ImageService;
    let Some(url) = cover_url.map(str::trim).filter(|url| !url.is_empty()) else {
        return (None, None);
    };
    if let Some(id) = ImageService::id_from_route(url) {
        return (Some(id.to_string()), None);
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        (Some(ImageService::image_key(url)), None)
    } else {
        (None, Some(url.to_string()))
    }
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BookDetail {
    /// 不透明 id（`book_<n>`）
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    /// 绑定书源；null 表示本地书
    pub source_id: Option<String>,
    /// 上游地址：只读诊断字段（红线 6 例外之外的只读输出）
    pub book_url: String,
    pub intro: Option<String>,
    pub kind: Option<String>,
    /// 封面在图片管道中的 id（md5(去查询串地址)）；取图地址随 images 域
    /// 切换迁移（当前 /reader3/image/<id>）
    pub cover_image_id: Option<String>,
    /// 站内资源封面地址（本地 EPUB 的 `/reader3/localEpubAsset?...`）；
    /// 与 `coverImageId` 二选一，客户端直接渲染
    pub cover_url: Option<String>,
    /// 章节登记表的章节数（目录未抓时缺省）
    pub total_chapters: Option<u32>,
    pub last_chapter: Option<LastChapter>,
    /// 上游详情获取失败，当前是登记行回退数据
    pub stale: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LastChapter {
    pub index: i32,
    pub title: String,
}

use axum::Json;

// ─── 书架读取（v1） ─────────────────────────────────────────────────────

const DEFAULT_SHELF_LIMIT: u32 = 50;
const MAX_SHELF_LIMIT: u32 = 200;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListShelfQuery {
    /// 上一页返回的 nextCursor；空表示第一页
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// 书架页（按 position 升序）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShelfPage {
    pub items: Vec<ShelfItem>,
    pub next_cursor: Option<String>,
    pub total: u32,
}

/// 书架条目：书的注册信息快照 + 我的进度/分组。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShelfItem {
    pub book_id: String,
    /// 绑定书源（`src_<n>`）；null 表示本地书
    pub source_id: Option<String>,
    pub name: String,
    pub author: String,
    /// 上游地址：只读诊断字段（红线 6 例外）
    pub book_url: String,
    pub origin: String,
    pub origin_name: Option<String>,
    /// 封面在图片管道中的 id；客户端取图走 `/reader3/image/<id>`
    pub cover_image_id: Option<String>,
    /// 站内资源封面地址（本地 EPUB）；与 `coverImageId` 二选一
    pub cover_url: Option<String>,
    /// 用户自设封面（任意地址，原样透传）
    pub custom_cover_url: Option<String>,
    pub intro: Option<String>,
    pub kind: Option<String>,
    pub charset: Option<String>,
    pub can_update: Option<bool>,
    /// 分组位掩码（与阅读3.0 约定一致，前端按位与过滤）
    pub group: i64,
    pub progress: ShelfProgress,
    /// 最近阅读时间，RFC 3339 UTC；未读过为 null
    #[schema(format = DateTime)]
    pub last_read_at: Option<String>,
    pub latest_chapter_title: Option<String>,
    pub total_chapters: Option<u32>,
    pub word_count: Option<String>,
    #[schema(format = DateTime)]
    pub last_check_at: Option<String>,
    /// 已缓存正文的章节数（本地书恒 0）
    pub cached_chapters: u32,
    /// 书架排序位置（升序）
    pub position: i64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShelfProgress {
    pub chapter_index: i32,
    pub chapter_title: Option<String>,
    /// 章内进度 0..1（存储是万分比）
    pub offset: f64,
}

#[derive(Serialize, Deserialize)]
struct ShelfCursorPayload {
    position: i64,
    book_id: String,
}

fn encode_shelf_cursor(position: i64, book_id: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&ShelfCursorPayload {
            position,
            book_id: book_id.to_string(),
        })
        .expect("shelf cursor json"),
    )
}

fn decode_shelf_cursor(raw: &str) -> Result<(i64, String), Problem> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| Problem::new(ErrorCode::InvalidCursor).detail("游标无法解码"))?;
    let payload: ShelfCursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| Problem::new(ErrorCode::InvalidCursor).detail("游标无法解码"))?;
    Ok((payload.position, payload.book_id))
}

fn is_local_shelf_book(book: &Book) -> bool {
    use crate::service::local_epub_book::is_local_epub_url;
    use crate::service::local_txt_book::is_local_txt_url;
    is_local_txt_url(&book.book_url) || is_local_epub_url(&book.book_url)
}

async fn build_shelf_item(
    state: &AppState,
    user_ns: &str,
    book: &Book,
    position: i64,
) -> Result<ShelfItem, Problem> {
    let book_id = book.book_id.clone().unwrap_or_default();
    // 缓存章数 = 正文池里这本书的章节数：本地书恒 0；网络书直接 COUNT——
    // 没有登记目录时 count 本来就是 0（r3 的 exists 预探测是冗余查询，还
    // 会把「有缓存正文但登记行被清」的书写成 0）。列表不做目录后台预取
    let cached_chapters = if is_local_shelf_book(book) {
        0
    } else {
        state
            .book_service
            .cached_chapter_count(user_ns, &book_id)
            .await
            .map_err(Problem::from)? as u32
    };
    let offset = f64::from(book.dur_chapter_pos.unwrap_or(0).clamp(0, 10_000)) / 10_000.0;
    let (cover_image_id, cover_url) = split_cover(book.cover_url.as_deref());
    Ok(ShelfItem {
        book_id,
        source_id: book.book_source_id.map(|id| format!("src_{id}")),
        name: book.name.clone(),
        author: book.author.clone(),
        book_url: book.book_url.clone(),
        origin: book.origin.clone(),
        origin_name: book.origin_name.clone(),
        cover_image_id,
        cover_url,
        custom_cover_url: book.custom_cover_url.clone(),
        intro: book.intro.clone(),
        kind: book.kind.clone(),
        charset: book.charset.clone(),
        can_update: book.can_update,
        group: book.group.unwrap_or(0),
        progress: ShelfProgress {
            chapter_index: book.dur_chapter_index.unwrap_or(0),
            chapter_title: book.dur_chapter_title.clone(),
            offset,
        },
        last_read_at: book.dur_chapter_time.map(crate::api::v1::me::rfc3339_secs),
        latest_chapter_title: book.latest_chapter_title.clone(),
        total_chapters: book
            .total_chapter_num
            .and_then(|count| u32::try_from(count).ok()),
        word_count: book.word_count.clone(),
        last_check_at: book.last_check_time.map(crate::api::v1::me::rfc3339_secs),
        cached_chapters,
        position,
    })
}

/// 我的书架（按 position 升序，游标分页）。
#[utoipa::path(
    get,
    path = "/v1/me/shelf",
    tag = "me",
    operation_id = "listShelf",
    params(
        ("cursor" = Option<String>, Query, description = "上一页的 nextCursor"),
        ("limit" = Option<u32>, Query, description = "页大小，默认 50，上限 200"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "书架页", body = ShelfPage),
        (status = 304, description = "If-None-Match 命中，无 body"),
        (status = 400, description = "limit 超上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "游标无法解码", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_shelf(
    State(state): State<AppState>,
    V1User(user): V1User,
    Query(query): Query<ListShelfQuery>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let limit = query.limit.unwrap_or(DEFAULT_SHELF_LIMIT);
    if limit == 0 || limit > MAX_SHELF_LIMIT {
        return Err(Problem::new(ErrorCode::MalformedRequest)
            .detail(format!("limit 必须在 1..={MAX_SHELF_LIMIT} 之间")));
    }
    let cursor = match query.cursor.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(decode_shelf_cursor(raw)?),
        _ => None,
    };

    let all = state
        .book_service
        .get_bookshelf_ordered(&user.ns)
        .await
        .map_err(Problem::from)?;
    let total = all.len() as u32;

    let mut selected: Vec<(Book, i64)> = Vec::new();
    for (book, position) in all {
        let after_cursor = match &cursor {
            None => true,
            Some((cursor_position, cursor_book_id)) => {
                position > *cursor_position
                    || (position == *cursor_position
                        && book.book_id.as_deref().unwrap_or("") > cursor_book_id.as_str())
            }
        };
        if after_cursor {
            selected.push((book, position));
        }
    }
    let has_more = selected.len() as u32 > limit;
    selected.truncate(limit as usize);

    let mut items = Vec::with_capacity(selected.len());
    for (book, position) in &selected {
        items.push(build_shelf_item(&state, &user.ns, book, *position).await?);
    }
    let next_cursor = has_more
        .then(|| {
            items
                .last()
                .map(|last| encode_shelf_cursor(last.position, &last.book_id))
        })
        .flatten();

    let page = ShelfPage {
        items,
        next_cursor,
        total,
    };
    let body = serde_json::to_vec(&page).expect("shelf page serializes");
    let etag = crate::api::etag::strong_etag(&body);
    let mut response =
        if crate::api::etag::if_none_match_matches(headers.get(header::IF_NONE_MATCH), &etag) {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            ([(header::CONTENT_TYPE, "application/json")], body).into_response()
        };
    response.headers_mut().insert(header::ETAG, etag);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}

/// 单本书的书架条目（含缓存章数）。
#[utoipa::path(
    get,
    path = "/v1/me/shelf/{bookId}",
    tag = "me",
    operation_id = "getShelfItem",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "书架条目", body = ShelfItem),
        (status = 304, description = "If-None-Match 命中，无 body"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在（未登记）", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "书籍不在书架（BOOK_NOT_IN_SHELF）", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_shelf_item(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    // 未登记的 bookId 是 404「书籍不存在」；已登记但不在架是 409
    // BOOK_NOT_IN_SHELF——与 PATCH 同码，客户端据此区分「书没了」与「没入架」
    crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => book_not_found(),
            other => Problem::from(other),
        })?;
    let (book, position) = state
        .book_service
        .get_shelf_book_with_position(&user.ns, &book_id)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| Problem::new(ErrorCode::BookNotInShelf).detail("这本书还没有加入书架"))?;
    let item = build_shelf_item(&state, &user.ns, &book, position).await?;
    let body = serde_json::to_vec(&item).expect("shelf item serializes");
    let etag = crate::api::etag::strong_etag(&body);
    let mut response =
        if crate::api::etag::if_none_match_matches(headers.get(header::IF_NONE_MATCH), &etag) {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            ([(header::CONTENT_TYPE, "application/json")], body).into_response()
        };
    response.headers_mut().insert(header::ETAG, etag);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::split_cover;

    #[test]
    fn cover_split_routes_upstream_and_local_asset() {
        // 上游 http(s) 封面：只给图片管道 id（md5 恒定，不泄露会过期的地址）
        let upstream = "https://up.example/cover/1.jpeg?sign=abc";
        let (id, route) = split_cover(Some(upstream));
        assert_eq!(route, None);
        assert_eq!(
            id.as_deref(),
            Some(crate::service::image_service::ImageService::image_key(upstream).as_str())
        );

        // 已是本站取图地址：还原 id，不再二次哈希
        let id = "0123456789abcdef0123456789abcdef";
        let (parsed, route) = split_cover(Some(&format!("/reader3/image/{id}")));
        assert_eq!(parsed.as_deref(), Some(id));
        assert_eq!(route, None);

        // 本地 EPUB 封面：站内资源地址进不了图片管道，原样透传
        let local = "/reader3/localEpubAsset?bookUrl=local-epub%3Aabc&path=OEBPS%2Fcover.jpg";
        let (id, route) = split_cover(Some(local));
        assert_eq!(id, None);
        assert_eq!(route.as_deref(), Some(local));

        // 空串/空白：无封面（md5("") 会指向不存在的图）
        assert_eq!(split_cover(Some("")), (None, None));
        assert_eq!(split_cover(Some("   ")), (None, None));
        assert_eq!(split_cover(None), (None, None));
    }
}
