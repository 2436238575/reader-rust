use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::Value;

use crate::api::handlers::book::resolve_book_source;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::book_source::BookSource;
use crate::model::review::ReviewSort;
use crate::service::book_service::REVIEW_PAGE_SIZE;

/// 站点普遍限制单页评论条数（番茄是 50），统一在此收口。
const MAX_REVIEW_COUNT: i32 = 50;

/// 评论接口的公共入参。
///
/// 与正文接口一致，GET 查询串和 POST 表单/JSON 都支持：前端走 POST JSON，
/// 保留查询串形式是为了方便 curl 调试。
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReviewRequest {
    pub book_url: Option<String>,
    pub chapter_url: Option<String>,
    pub book_source_url: Option<String>,
    pub book_source: Option<Value>,
    pub page: Option<i32>,
    pub count: Option<i32>,
    pub para_index: Option<i32>,
    /// `hot`（默认，站点热度序）或 `time`（按时间倒序）
    pub sort: Option<String>,
    /// true 时忽略分页参数，返回「只看作者」扫描结果（作者评论/赞过/回复过的整条评论）
    pub author_only: Option<bool>,
    pub refresh: Option<i32>,
}

/// 一次评论查询的完整上下文。
struct ReviewContext {
    source: BookSource,
    book_url: String,
    chapter_url: String,
    page: i32,
    count: i32,
    sort: ReviewSort,
    refresh: bool,
}

/// 把 POST 表单参数合并进查询参数。
fn merge_body(mut req: ReviewRequest, body: &axum::body::Bytes) -> ReviewRequest {
    if body.is_empty() {
        return req;
    }
    if let Ok(v) = serde_json::from_slice::<ReviewRequest>(body) {
        if req.book_url.is_none() {
            req.book_url = v.book_url;
        }
        if req.chapter_url.is_none() {
            req.chapter_url = v.chapter_url;
        }
        if req.book_source_url.is_none() {
            req.book_source_url = v.book_source_url;
        }
        if req.book_source.is_none() {
            req.book_source = v.book_source;
        }
        if req.page.is_none() {
            req.page = v.page;
        }
        if req.count.is_none() {
            req.count = v.count;
        }
        if req.para_index.is_none() {
            req.para_index = v.para_index;
        }
        if req.sort.is_none() {
            req.sort = v.sort;
        }
        if req.author_only.is_none() {
            req.author_only = v.author_only;
        }
        if req.refresh.is_none() {
            req.refresh = v.refresh;
        }
        return req;
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return req;
    };
    for (key, value) in url::form_urlencoded::parse(text.as_bytes()) {
        match key.as_ref() {
            "bookUrl" | "url" => req.book_url = Some(value.into_owned()),
            "chapterUrl" | "href" => req.chapter_url = Some(value.into_owned()),
            "bookSourceUrl" | "origin" => req.book_source_url = Some(value.into_owned()),
            "page" => req.page = value.parse().ok(),
            "count" => req.count = value.parse().ok(),
            "paraIndex" => req.para_index = value.parse().ok(),
            "sort" => req.sort = Some(value.into_owned()),
            "authorOnly" => {
                req.author_only = Some(matches!(value.as_ref(), "1" | "true"))
            }
            "refresh" => req.refresh = value.parse().ok(),
            _ => {}
        }
    }
    req
}

/// 解析请求里的书源与 URL，得到一次评论查询的全部上下文。
async fn resolve_context(
    state: &AppState,
    user_ns: &str,
    req: ReviewRequest,
) -> Result<ReviewContext, AppError> {
    let book_url = required(&req.book_url, "bookUrl")?;
    let chapter_url = required(&req.chapter_url, "chapterUrl")?;
    let book_source = req
        .book_source
        .clone()
        .and_then(|v| serde_json::from_value::<BookSource>(v).ok());
    let source = resolve_book_source(
        state,
        user_ns,
        req.book_source_url.clone(),
        book_source,
        Some(&book_url),
    )
    .await?;
    Ok(ReviewContext {
        source,
        book_url,
        chapter_url,
        page: req.page.unwrap_or(1).max(1),
        count: req
            .count
            .unwrap_or(REVIEW_PAGE_SIZE)
            .clamp(1, MAX_REVIEW_COUNT),
        // 认不出来的取值一律按默认的「最热」，不报错
        sort: match req.sort.as_deref().map(str::trim) {
            Some("time") => ReviewSort::Time,
            _ => ReviewSort::Hot,
        },
        refresh: req.refresh.unwrap_or(0) > 0,
    })
}

fn required(value: &Option<String>, name: &str) -> Result<String, AppError> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppError::BadRequest(format!("{name} required")))
}

/// 评论配图收口到本站取图接口（签名会过期，id 不会），并带上书籍上下文。
///
/// 站点给的多个格式变体在这里挑一张（管道会把 HEIC 转成 JPEG），
/// 前端拿到的是单张 `/reader3/image/<id>`。
async fn ok_json_with_images<T: serde::Serialize>(
    state: &AppState,
    ctx: &ReviewContext,
    value: T,
) -> Json<ApiResponse<Value>> {
    let mut value = serde_json::to_value(value).unwrap_or_default();
    state
        .image_service
        .rewrite_image_lists(
            &mut value,
            "review",
            Some(&ctx.book_url),
            Some(&ctx.source.book_source_url),
        )
        .await;
    Json(ApiResponse::ok(value))
}

/// 章评：本章评论列表。
pub async fn get_chapter_comments(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<ReviewRequest>,
    body: axum::body::Bytes,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let req = merge_body(q, &body);
    let user_ns = user.0.ns.clone();
    let author_only = req.author_only.unwrap_or(false);
    let ctx = resolve_context(&state, &user_ns, req).await?;
    let result = if author_only {
        state
            .book_service
            .get_author_reviews(&user_ns, &ctx.source, &ctx.book_url, &ctx.chapter_url, None, ctx.refresh)
            .await?
    } else {
        state
            .book_service
            .get_chapter_reviews(
                &user_ns,
                &ctx.source,
                &ctx.book_url,
                &ctx.chapter_url,
                ctx.page,
                ctx.count,
                ctx.sort,
                ctx.refresh,
            )
            .await?
    };
    Ok(ok_json_with_images(&state, &ctx, result).await)
}

/// 段评概览：本章哪些段落有段评、各有多少条。
pub async fn get_para_comment_index(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<ReviewRequest>,
    body: axum::body::Bytes,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let req = merge_body(q, &body);
    let user_ns = user.0.ns.clone();
    let ctx = resolve_context(&state, &user_ns, req).await?;
    let result = state
        .book_service
        .get_para_review_index(
            &user_ns,
            &ctx.source,
            &ctx.book_url,
            &ctx.chapter_url,
            ctx.refresh,
        )
        .await?;
    Ok(ok_json_with_images(&state, &ctx, result).await)
}

/// 段评：指定段落的评论列表。
pub async fn get_para_comments(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<ReviewRequest>,
    body: axum::body::Bytes,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let req = merge_body(q, &body);
    let para_index = req
        .para_index
        .filter(|v| *v >= 0)
        .ok_or_else(|| AppError::BadRequest("paraIndex required".to_string()))?;
    let author_only = req.author_only.unwrap_or(false);
    let user_ns = user.0.ns.clone();
    let ctx = resolve_context(&state, &user_ns, req).await?;
    let result = if author_only {
        state
            .book_service
            .get_author_reviews(
                &user_ns,
                &ctx.source,
                &ctx.book_url,
                &ctx.chapter_url,
                Some(para_index),
                ctx.refresh,
            )
            .await?
    } else {
        state
            .book_service
            .get_para_reviews(
                &user_ns,
                &ctx.source,
                &ctx.book_url,
                &ctx.chapter_url,
                para_index,
                ctx.page,
                ctx.count,
                ctx.sort,
                ctx.refresh,
            )
            .await?
    };
    Ok(ok_json_with_images(&state, &ctx, result).await)
}
