//! 章节正文的用户版本、版本时间线与版本选择端点。
//!
//! 编辑版和手动导入的版本都进正文池（`chapter_versions.kind` 为 edited /
//! imported）；删除当前用户版本只清指针，历史仍然保留。

use crate::api::extract::QueryOrBody;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::storage::db::content_repo::RecordOutcome;
use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SaveChapterEditRequest {
    pub book_id: String,
    pub chapter_url: String,
    pub chapter_index: i32,
    pub chapter_title: String,
    pub content: String,
    /// 编辑时看到的上游版本；不传则取当前最新上游。
    pub base_version_id: Option<i64>,
    pub note: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterEditQuery {
    pub book_id: String,
    pub chapter_url: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterTimelineQuery {
    pub book_id: String,
    pub chapter_url: String,
    pub limit: Option<i64>,
    pub before_version_id: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterVersionQuery {
    pub book_id: String,
    pub chapter_url: String,
    pub version_id: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterVersionDiffQuery {
    pub book_id: String,
    pub chapter_url: String,
    pub from_version_id: i64,
    pub to_version_id: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SelectChapterVersionRequest {
    pub book_id: String,
    pub chapter_url: String,
    /// 空表示「跟随最新」（删除手动选择）。
    pub version_id: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportChapterVersionRequest {
    pub book_id: String,
    pub chapter_url: String,
    pub content: String,
    pub chapter_index: Option<i32>,
    pub chapter_title: Option<String>,
    pub base_version_id: Option<i64>,
    pub note: Option<String>,
    /// 默认只进时间线，不改变当前显示。
    pub make_current: Option<bool>,
    /// 手动补历史时可指定时间（毫秒）；不传用当前时间。
    pub created_at: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterTimelineItem {
    pub version_id: i64,
    pub kind: String,
    pub created_at: i64,
    pub raw_size: i64,
    pub chapter_index: i32,
    pub chapter_title: String,
    pub note: String,
    pub book_source_id: Option<i64>,
    pub book_source_name: String,
    pub base_version_id: Option<i64>,
    pub is_current: bool,
    pub is_latest_upstream: bool,
    pub is_latest_user: bool,
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

async fn resolve_book_checked(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
) -> Result<super::book::ResolvedBook, AppError> {
    super::book::resolve_book(state, user_ns, book_id).await
}

fn outcome_version_id(outcome: RecordOutcome) -> i64 {
    match outcome {
        RecordOutcome::Unchanged { version_id } => version_id,
        RecordOutcome::Appended { version_id, .. } => version_id,
    }
}

fn outcome_became_current(outcome: &RecordOutcome) -> bool {
    matches!(
        outcome,
        RecordOutcome::Appended {
            became_current: true,
            ..
        }
    )
}

async fn validate_base(
    state: &AppState,
    book_id: &str,
    chapter_url: &str,
    base_version_id: Option<i64>,
) -> Result<Option<i64>, AppError> {
    let Some(base) = base_version_id else {
        return Ok(None);
    };
    let exists = state
        .chapter_content_service
        .version(book_id, chapter_url, base)
        .await?;
    if exists.is_none() {
        return Err(AppError::BadRequest("baseVersionId 不属于本章".to_string()));
    }
    Ok(Some(base))
}

pub async fn save_chapter_edit(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<SaveChapterEditRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    let resolved = resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    let base_version_id = match req.base_version_id {
        Some(base) => validate_base(&state, &req.book_id, &req.chapter_url, Some(base)).await?,
        None => state
            .chapter_content_service
            .latest(&req.book_id, &req.chapter_url)
            .await?
            .and_then(|row| row.latest_upstream_version_id),
    };
    let outcome = state
        .chapter_content_service
        .save_user_version(
            &user.0.ns,
            &req.book_id,
            &req.chapter_url,
            "edited",
            &req.content,
            base_version_id,
            req.chapter_index,
            &req.chapter_title,
            &resolved.book_url,
            req.note.as_deref().unwrap_or(""),
            true,
            None,
        )
        .await?;
    Ok(Json(ApiResponse::ok(json!({
        "edited": true,
        "versionId": outcome_version_id(outcome),
    }))))
}

pub async fn get_chapter_edit(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ChapterEditQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    let view = state
        .chapter_content_service
        .latest_user_version(&req.book_id, &req.chapter_url)
        .await?;
    match view {
        Some(view) => Ok(Json(ApiResponse::ok(json!({
            "content": view.content,
            "versionId": view.version_id,
            "baseVersionId": view.base_version_id,
            "kind": view.kind,
        })))),
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
    resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    let urls = state
        .chapter_content_service
        .list_user_version_urls(&user.0.ns, &req.book_id)
        .await?;
    Ok(Json(ApiResponse::ok(json!({ "urls": urls }))))
}

pub async fn delete_chapter_edit(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ChapterEditQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    state
        .chapter_content_service
        .clear_current_user_version(&user.0.ns, &req.book_id, &req.chapter_url)
        .await?;
    Ok(Json(ApiResponse::ok(json!({ "edited": false }))))
}

/// 某章的版本时间线（倒序，元数据不含正文）。
pub async fn get_chapter_timeline(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ChapterTimelineQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    let latest = state
        .chapter_content_service
        .latest(&req.book_id, &req.chapter_url)
        .await?;
    let selected = state
        .chapter_content_service
        .selected_version_id(&user.0.ns, &req.book_id, &req.chapter_url)
        .await?;
    let effective = selected
        .or_else(|| latest.as_ref().and_then(|row| row.latest_user_version_id))
        .or_else(|| {
            latest
                .as_ref()
                .and_then(|row| row.latest_upstream_version_id)
        });
    let rows = state
        .chapter_content_service
        .timeline_meta(
            &req.book_id,
            &req.chapter_url,
            req.limit.unwrap_or(50),
            req.before_version_id,
        )
        .await?;
    let items: Vec<ChapterTimelineItem> = rows
        .iter()
        .map(|row| ChapterTimelineItem {
            version_id: row.version_id,
            kind: row.kind.clone(),
            created_at: row.created_at,
            raw_size: row.raw_size,
            chapter_index: row.chapter_index,
            chapter_title: row.chapter_title.clone(),
            note: row.note.clone(),
            book_source_id: row.book_source_id,
            book_source_name: row.book_source_name.clone(),
            base_version_id: row.base_version_id,
            is_current: effective == Some(row.version_id),
            is_latest_upstream: latest
                .as_ref()
                .and_then(|latest| latest.latest_upstream_version_id)
                == Some(row.version_id),
            is_latest_user: latest
                .as_ref()
                .and_then(|latest| latest.latest_user_version_id)
                == Some(row.version_id),
        })
        .collect();
    Ok(Json(ApiResponse::ok(json!({
        "items": items,
        "latestUpstreamVersionId": latest.as_ref().and_then(|row| row.latest_upstream_version_id),
        "latestUserVersionId": latest.as_ref().and_then(|row| row.latest_user_version_id),
        "selectedVersionId": selected,
        "effectiveVersionId": effective,
    }))))
}

/// 取某个版本的正文（预览用，不改变当前选择）。
pub async fn get_chapter_version(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ChapterVersionQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    let view = state
        .chapter_content_service
        .version(&req.book_id, &req.chapter_url, req.version_id)
        .await?
        .ok_or_else(|| AppError::NotFound("章节版本不存在".to_string()))?;
    Ok(Json(ApiResponse::ok(json!({
        "versionId": view.version_id,
        "kind": view.kind,
        "baseVersionId": view.base_version_id,
        "createdAt": view.created_at,
        "rawSize": view.raw_size,
        "note": view.note,
        "bookSourceName": view.book_source_name,
        "chapterIndex": view.chapter_index,
        "chapterTitle": view.chapter_title,
        "content": view.content,
    }))))
}

/// 任意两个版本之间的 diff。
pub async fn get_chapter_version_diff(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ChapterVersionDiffQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    let from = state
        .chapter_content_service
        .version(&req.book_id, &req.chapter_url, req.from_version_id)
        .await?
        .ok_or_else(|| AppError::NotFound("起始版本不存在".to_string()))?;
    let to = state
        .chapter_content_service
        .version(&req.book_id, &req.chapter_url, req.to_version_id)
        .await?
        .ok_or_else(|| AppError::NotFound("目标版本不存在".to_string()))?;
    let ops = state
        .git_diff_service
        .diff_texts(&from.content, &to.content)
        .await
        .map_err(|reason| AppError::BadRequest(format!("diff 不可用: {reason}")))?;
    Ok(Json(ApiResponse::ok(json!({
        "fromVersionId": from.version_id,
        "toVersionId": to.version_id,
        "ops": ops,
    }))))
}

/// 设置当前显示版本；`versionId` 为空表示跟随最新。
pub async fn select_chapter_version(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<SelectChapterVersionRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    state
        .chapter_content_service
        .select_version(&user.0.ns, &req.book_id, &req.chapter_url, req.version_id)
        .await?;
    let latest = state
        .chapter_content_service
        .latest(&req.book_id, &req.chapter_url)
        .await?;
    let effective = req
        .version_id
        .or_else(|| latest.as_ref().and_then(|row| row.latest_user_version_id))
        .or_else(|| {
            latest
                .as_ref()
                .and_then(|row| row.latest_upstream_version_id)
        });
    Ok(Json(ApiResponse::ok(json!({
        "selectedVersionId": req.version_id,
        "effectiveVersionId": effective,
    }))))
}

/// 手动导入一个历史版本。默认只进时间线，不改变当前显示。
pub async fn import_chapter_version(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ImportChapterVersionRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require_ids(&req.book_id, &req.chapter_url)?;
    let resolved = resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    let base_version_id =
        validate_base(&state, &req.book_id, &req.chapter_url, req.base_version_id).await?;
    let make_current = req.make_current.unwrap_or(false);
    let outcome = state
        .chapter_content_service
        .save_user_version(
            &user.0.ns,
            &req.book_id,
            &req.chapter_url,
            "imported",
            &req.content,
            base_version_id,
            req.chapter_index.unwrap_or(0),
            req.chapter_title.as_deref().unwrap_or(""),
            &resolved.book_url,
            req.note.as_deref().unwrap_or(""),
            make_current,
            req.created_at,
        )
        .await?;
    Ok(Json(ApiResponse::ok(json!({
        "versionId": outcome_version_id(outcome.clone()),
        "becameCurrent": outcome_became_current(&outcome),
    }))))
}
