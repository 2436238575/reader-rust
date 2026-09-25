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

/// 章节配图接口的入参。
///
/// 与正文接口一致：GET 查询串和 POST 表单/JSON 都支持，前端走 POST JSON，
/// 保留查询串形式是为了方便 curl 调试。
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterImageRequest {
    pub book_url: Option<String>,
    pub chapter_url: Option<String>,
    pub book_source_url: Option<String>,
    pub book_source: Option<Value>,
}

/// 把 POST 表单参数合并进查询参数。
fn merge_body(mut req: ChapterImageRequest, body: &axum::body::Bytes) -> ChapterImageRequest {
    if body.is_empty() {
        return req;
    }
    if let Ok(v) = serde_json::from_slice::<ChapterImageRequest>(body) {
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
            _ => {}
        }
    }
    req
}

fn required(value: &Option<String>, name: &str) -> Result<String, AppError> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppError::BadRequest(format!("{name} required")))
}

/// 本章配图。
///
/// 书源没声明配图规则时返回 `enabled: false`；「这一章没有配图」是
/// `enabled: true` + 空列表，不是错误——前端靠 `enabled` 决定要不要渲染配图区。
pub async fn get_chapter_images(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<ChapterImageRequest>,
    body: axum::body::Bytes,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let req = merge_body(q, &body);
    let user_ns = user.0.ns.clone();
    let book_url = required(&req.book_url, "bookUrl")?;
    let chapter_url = required(&req.chapter_url, "chapterUrl")?;
    let book_source = req
        .book_source
        .clone()
        .and_then(|v| serde_json::from_value::<BookSource>(v).ok());
    let source = resolve_book_source(
        &state,
        &user_ns,
        req.book_source_url.clone(),
        book_source,
        Some(&book_url),
    )
    .await?;
    let result = state
        .book_service
        .get_chapter_images(&user_ns, &source, &chapter_url)
        .await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(result).unwrap_or_default(),
    )))
}
