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
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::Router;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 「最近阅读时间」刷新节流：窗口内的重复保存不改动时间戳。
const PROGRESS_TIME_THROTTLE_SECS: i64 = 60;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/me/shelf/{bookId}",
            put(add_to_shelf)
                .patch(patch_shelf_book)
                .delete(remove_from_shelf),
        )
        .route("/v1/books/{bookId}", get(get_book).patch(patch_book))
}

fn book_not_found() -> Problem {
    Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
}

fn book_not_in_shelf() -> Problem {
    Problem::new(ErrorCode::BookNotInShelf).detail("这本书还没有加入书架")
}

/// 入架（幂等）：已登记的书籍确保在架。201 首次 / 200 已在架。
///
/// 元数据用书籍详情（1h 缓存，详情页刚看过时命中）填充；冷缓存会回源一次
/// 上游——与打开详情页的成本相同。
#[utoipa::path(
    put,
    path = "/v1/me/shelf/{bookId}",
    tag = "me",
    operation_id = "addToShelf",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 201, description = "已入架", body = ShelfWriteResult),
        (status = 200, description = "已在架（幂等重复调用，不重置进度）", body = ShelfWriteResult),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn add_to_shelf(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
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

    // 元数据用书籍详情（1h 缓存，详情页刚看过时命中）填充；本地书走本地详情。
    // 详情拿不到（源挂了/离线）不阻塞入架：回退登记行信息，条目先立起来
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

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShelfWriteResult {
    pub book_id: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShelfPatch {
    pub progress: Option<ProgressPatch>,
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
    let Some(progress) = patch.progress else {
        // merge-patch 空体：合法但无事可做
        return Ok(StatusCode::NO_CONTENT);
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
    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => book_not_found(),
            other => Problem::from(other),
        })?;
    // 详情拿不到（源挂了/离线）不阻塞读取：回退登记行信息并标 stale，
    // 详情页永远能打开（比 /reader3 的 502 行为更好）
    let (info, stale) = match state
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
    };
    // 封面走图片管道：coverImageId = md5(去查询串的上游地址)，同一张图
    // 永远同一个 id——签名的上游直出地址会过期，不能给客户端
    let cover_image_id = info
        .cover_url
        .as_deref()
        .map(crate::service::image_service::ImageService::image_key);
    let chapters = state
        .book_service
        .load_chapter_list_cache(&user.ns, &book_id)
        .await
        .map_err(Problem::from)?
        .unwrap_or_default();
    let total_chapters = (!chapters.is_empty()).then_some(chapters.len() as u32);
    let last_chapter = chapters.last().map(|chapter| LastChapter {
        index: chapter.index,
        title: chapter.title.clone(),
    });

    Ok(axum::Json(BookDetail {
        id: book_id.clone(),
        name: info.name,
        author: Some(info.author).filter(|value| !value.trim().is_empty()),
        source_id: info.book_source_id.map(|id| format!("src_{id}")),
        book_url: resolved.book_url.clone(),
        intro: info.intro,
        kind: info.kind,
        cover_image_id,
        total_chapters,
        last_chapter,
        stale,
    }))
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
