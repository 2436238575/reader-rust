//! 章节正文编辑版的读写端点。
//!
//! 编辑版存在 `data/<ns>/edited_chapters/`（用户资产区，与可再生缓存分离）：
//! 刷新上游只更新缓存基准，不动编辑版；删除编辑版即恢复上游版本。

use crate::api::extract::QueryOrBody;
use crate::api::AppState;
use crate::auth::CurrentUser;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::error::{ApiResponse, AppError};

#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SaveChapterEditRequest {
    pub book_id: String,
    pub chapter_url: String,
    pub chapter_index: i32,
    pub chapter_title: String,
    pub content: String,
}

impl Default for SaveChapterEditRequest {
    fn default() -> Self {
        Self {
            book_id: String::new(),
            chapter_url: String::new(),
            chapter_index: 0,
            chapter_title: String::new(),
            content: String::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterEditQuery {
    pub book_id: String,
    pub chapter_url: String,
}

fn require_ids(book_id: &str, chapter_url: &str) -> Result<(), AppError> {
    if book_id.trim().is_empty() {
        return Err(AppError::BadRequest("bookId required".to_string()));
    }
    if chapter_url.trim().is_empty() {
        return Err(AppError::BadRequest("chapterUrl required".to_string()));
    }
    Ok(())
}

pub async fn save_chapter_edit(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<SaveChapterEditRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    state
        .chapter_edit_service
        .save_chapter_edit(
            &user.0.ns,
            &req.book_id,
            &req.chapter_url,
            req.chapter_index,
            &req.chapter_title,
            &req.content,
        )
        .await?;
    Ok(Json(ApiResponse::ok(json!({ "edited": true }))))
}

pub async fn get_chapter_edit(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ChapterEditQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    let content = state
        .chapter_edit_service
        .get_chapter_edit(&user.0.ns, &req.book_id, &req.chapter_url)
        .await?;
    match content {
        Some(content) => Ok(Json(ApiResponse::ok(json!({ "content": content })))),
        None => Ok(Json(ApiResponse::ok(Value::Null))),
    }
}

pub async fn list_chapter_edits(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ChapterEditQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if req.book_id.trim().is_empty() {
        return Err(AppError::BadRequest("bookId required".to_string()));
    }
    let urls = state
        .chapter_edit_service
        .list_chapter_edits(&user.0.ns, &req.book_id)
        .await;
    Ok(Json(ApiResponse::ok(json!({ "urls": urls }))))
}

pub async fn delete_chapter_edit(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ChapterEditQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    state
        .chapter_edit_service
        .delete_chapter_edit(&user.0.ns, &req.book_id, &req.chapter_url)
        .await?;
    Ok(Json(ApiResponse::ok(json!({ "edited": false }))))
}
