use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::Value;

use crate::api::handlers::book::resolve_book;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::service::image_service::ImageService;

/// 章节配图接口的入参。
///
/// 与正文接口一致：GET 查询串和 POST 表单/JSON 都支持，前端走 POST JSON，
/// 保留查询串形式是为了方便 curl 调试。
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterImageRequest {
    pub book_id: Option<String>,
    pub chapter_url: Option<String>,
}

/// 把 POST 表单参数合并进查询参数。
fn merge_body(mut req: ChapterImageRequest, body: &axum::body::Bytes) -> ChapterImageRequest {
    if body.is_empty() {
        return req;
    }
    if let Ok(v) = serde_json::from_slice::<ChapterImageRequest>(body) {
        if req.book_id.is_none() {
            req.book_id = v.book_id;
        }
        if req.chapter_url.is_none() {
            req.chapter_url = v.chapter_url;
        }
        return req;
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return req;
    };
    for (key, value) in url::form_urlencoded::parse(text.as_bytes()) {
        match key.as_ref() {
            "bookId" => req.book_id = Some(value.into_owned()),
            "chapterUrl" => req.chapter_url = Some(value.into_owned()),
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
    let book_id = required(&req.book_id, "bookId")?;
    let chapter_url = required(&req.chapter_url, "chapterUrl")?;
    let resolved = resolve_book(&state, &user_ns, &book_id).await?;
    let source = resolved.source;
    let mut result = state
        .book_service
        .get_chapter_images(&user_ns, &source, &chapter_url)
        .await?;
    // 图片地址收口到本站取图接口：签名会过期，id 不会
    for image in result.images.iter_mut() {
        if let Some(id) = state
            .image_service
            .register("chapter", &image.url, Some(&book_id))
            .await
        {
            image.url = ImageService::route(&id);
        }
    }
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(result).unwrap_or_default(),
    )))
}
