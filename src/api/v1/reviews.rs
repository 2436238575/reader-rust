//! `/v1/books/{bookId}/chapters/{index}/comments|images`：章评/段评/配图读链路。
//!
//! 与 `/reader3` 共用同一套解析与缓存服务；差异在对外契约：
//! - **身份是 `(bookId, index)`**：index 经章节登记表/本地目录解析出内部
//!   chapterUrl（红线 6），客户端不再携带上游地址；
//! - 章评与段评合并在同一路径：`?para=<block 下标>` 是段评，缺省是章评；
//!   `?authorOnly=true` 忽略分页扫描作者评论（`comments/index` 是段评概览）；
//! - 响应就是服务层的 typed 形状（`ReviewResponse<ReviewPage>` 等），
//!   评论配图/章节配图在出口收口成单个 `/v1/images/<id>`。
//!
//! 契约 §8.5 的 `anchor`（blockMap 对齐）随 mark-anchoring 批次再做；
//! 现状形状与 `/reader3` 一致，前端零映射迁移。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::V1User;
use crate::api::AppState;
use crate::model::chapter_image::ChapterImages;
use crate::model::review::ReviewSort;
use crate::model::review::{ParaReviewIndex, ReviewPage, ReviewResponse};
use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

/// 站点普遍限制单页评论条数（番茄是 50），与 /reader3 同一收口。
const MAX_REVIEW_COUNT: i32 = 50;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/books/{bookId}/chapters/{index}/comments",
            get(get_comments),
        )
        .route(
            "/v1/books/{bookId}/chapters/{index}/comments/index",
            get(get_comment_index),
        )
        .route(
            "/v1/books/{bookId}/chapters/{index}/images",
            get(get_chapter_images),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentsQuery {
    pub page: Option<i32>,
    pub count: Option<i32>,
    /// `hot`（默认，站点热度序）或 `time`（按时间倒序）；认不出的按 hot
    pub sort: Option<String>,
    /// true 时忽略分页，扫描作者评论/赞过/回复过的整条评论
    pub author_only: Option<bool>,
    /// 忽略 7 天评论缓存重抓
    pub refresh: Option<bool>,
    /// 正文版本：`edited`（默认）或 `upstream`。评论按版本分开缓存
    pub variant: Option<String>,
    /// 段号（正文按 `\n` 切分的下标）：提供即查段评，缺省查章评
    pub para: Option<i32>,
}

/// 一次评论查询的完整上下文（与 /reader3 的 resolve_context 同口径）。
struct CommentsContext {
    page: i32,
    count: i32,
    sort: ReviewSort,
    refresh: bool,
    variant: String,
}

fn build_context(query: &CommentsQuery) -> CommentsContext {
    CommentsContext {
        page: query.page.unwrap_or(1).max(1),
        count: query
            .count
            .unwrap_or(crate::service::book_service::REVIEW_PAGE_SIZE)
            .clamp(1, MAX_REVIEW_COUNT),
        sort: match query.sort.as_deref().map(str::trim) {
            Some("time") => ReviewSort::Time,
            _ => ReviewSort::Hot,
        },
        refresh: query.refresh.unwrap_or(false),
        variant: match query.variant.as_deref().map(str::trim) {
            Some(value) if value.eq_ignore_ascii_case("upstream") => "upstream".to_string(),
            _ => "edited".to_string(),
        },
    }
}

fn para_index_of(query: &CommentsQuery) -> Result<Option<i32>, Problem> {
    match query.para {
        // 段号必须是 >=0 的下标
        Some(value) if value >= 0 => Ok(Some(value)),
        Some(_) => Err(Problem::new(ErrorCode::MalformedRequest).detail("para 必须是 >=0 的段号")),
        None => Ok(None),
    }
}

/// 评论配图变体收口：同一张图的多个格式变体挑一张能渲染的，
/// 替换成单个 `/v1/images/<id>`（与 /reader3 的 rewrite_image_lists 同语义）。
async fn collapse_review_images(
    state: &AppState,
    book_id: &str,
    mut response: ReviewResponse<ReviewPage>,
) -> ReviewResponse<ReviewPage> {
    for item in &mut response.data.items {
        if let Some(route) = state
            .image_service
            .pick_image_route(&item.images, "review", Some(book_id))
            .await
        {
            item.images = vec![route];
        }
        for reply in &mut item.replies {
            if let Some(route) = state
                .image_service
                .pick_image_route(&reply.images, "review", Some(book_id))
                .await
            {
                reply.images = vec![route];
            }
        }
    }
    response
}

/// 章评 / 段评（`?para=`）；`?authorOnly=true` 忽略分页扫描作者评论。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters/{index}/comments",
    tag = "books",
    operation_id = "getChapterComments",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号（登记表分配的稳定序号）"),
        ("para" = Option<i32>, Query, description = "段号（>=0）：提供即查段评，缺省查章评"),
        ("page" = Option<i32>, Query, description = "页码，从 1 起"),
        ("count" = Option<i32>, Query, description = "每页条数，默认 20，上限 50"),
        ("sort" = Option<String>, Query, description = "hot（默认）或 time"),
        ("authorOnly" = Option<bool>, Query, description = "忽略分页扫描作者评论"),
        ("refresh" = Option<bool>, Query, description = "忽略 7 天评论缓存重抓"),
        ("variant" = Option<String>, Query, description = "edited（默认）或 upstream"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "评论页（enabled=false 表示书源没声明评论规则）", body = ReviewResponse<ReviewPage>),
        (status = 400, description = "para 取值非法", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍或章节不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_comments(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
    Query(query): Query<CommentsQuery>,
) -> Result<Json<ReviewResponse<ReviewPage>>, Problem> {
    let para_index = para_index_of(&query)?;
    let author_only = query.author_only.unwrap_or(false);
    let ctx = build_context(&query);
    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;
    let chapter_url =
        super::content::resolve_chapter_url(&state, &user.ns, &book_id, &resolved, index).await?;

    let result = if let Some(para_index) = para_index {
        if author_only {
            state
                .book_service
                .get_author_reviews(
                    &user.ns,
                    &resolved.source,
                    &book_id,
                    &resolved.book_url,
                    &chapter_url,
                    Some(para_index),
                    &ctx.variant,
                    ctx.refresh,
                )
                .await
                .map_err(Problem::from)?
        } else {
            state
                .book_service
                .get_para_reviews(
                    &user.ns,
                    &resolved.source,
                    &book_id,
                    &resolved.book_url,
                    &chapter_url,
                    para_index,
                    ctx.page,
                    ctx.count,
                    ctx.sort,
                    &ctx.variant,
                    ctx.refresh,
                )
                .await
                .map_err(Problem::from)?
        }
    } else if author_only {
        state
            .book_service
            .get_author_reviews(
                &user.ns,
                &resolved.source,
                &book_id,
                &resolved.book_url,
                &chapter_url,
                None,
                &ctx.variant,
                ctx.refresh,
            )
            .await
            .map_err(Problem::from)?
    } else {
        state
            .book_service
            .get_chapter_reviews(
                &user.ns,
                &resolved.source,
                &book_id,
                &resolved.book_url,
                &chapter_url,
                ctx.page,
                ctx.count,
                ctx.sort,
                &ctx.variant,
                ctx.refresh,
            )
            .await
            .map_err(Problem::from)?
    };
    Ok(Json(collapse_review_images(&state, &book_id, result).await))
}

/// 段评概览：本章哪些段落有段评、各有多少条。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters/{index}/comments/index",
    tag = "books",
    operation_id = "getCommentIndex",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
        ("refresh" = Option<bool>, Query, description = "忽略缓存重抓"),
        ("variant" = Option<String>, Query, description = "edited（默认）或 upstream"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "段评概览", body = ReviewResponse<ParaReviewIndex>),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍或章节不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_comment_index(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
    Query(query): Query<CommentsQuery>,
) -> Result<Json<ReviewResponse<ParaReviewIndex>>, Problem> {
    let ctx = build_context(&query);
    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;
    let chapter_url =
        super::content::resolve_chapter_url(&state, &user.ns, &book_id, &resolved, index).await?;
    let result = state
        .book_service
        .get_para_review_index(
            &user.ns,
            &resolved.source,
            &book_id,
            &resolved.book_url,
            &chapter_url,
            &ctx.variant,
            ctx.refresh,
        )
        .await
        .map_err(Problem::from)?;
    Ok(Json(result))
}

/// 本章配图（书源没声明配图规则时 `enabled: false`）；地址收口成单个
/// `/v1/images/<id>`。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters/{index}/images",
    tag = "books",
    operation_id = "getChapterImages",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "配图列表", body = ChapterImages),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍或章节不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_chapter_images(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
) -> Result<Json<ChapterImages>, Problem> {
    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;
    let chapter_url =
        super::content::resolve_chapter_url(&state, &user.ns, &book_id, &resolved, index).await?;
    let mut result = state
        .book_service
        .get_chapter_images(&user.ns, &resolved.source, &chapter_url)
        .await
        .map_err(Problem::from)?;
    for image in result.images.iter_mut() {
        if let Some(id) = state
            .image_service
            .register("chapter", &image.url, Some(&book_id))
            .await
        {
            image.url = crate::service::image_service::ImageService::route(&id);
        }
    }
    Ok(Json(result))
}
