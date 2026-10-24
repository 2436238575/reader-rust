//! 章节正文的用户版本、版本时间线与版本选择端点。
//!
//! 编辑版和手动导入的版本都进正文池（`chapter_versions.kind` 为 edited /
//! imported）；删除当前用户版本只清指针，历史仍然保留。

use crate::api::extract::QueryOrBody;
use crate::api::handlers::multipart::{next_field_limited, read_limited_multipart_field};
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::book_chapter::BookChapter;
use crate::service::chapter_content_service::normalized_content_hash;
use crate::service::chapter_text_import::{
    match_chapters, split_text, ImportMode, MatchedChapter, MAX_IMPORT_CHAPTERS,
};
use crate::service::local_epub_book::is_local_epub_url;
use crate::service::local_txt_book::{decode_txt_bytes, is_local_txt_url, MAX_TXT_UPLOAD_BYTES};
use crate::storage::db::content_repo::RecordOutcome;
use crate::util::text::repair_encoded_url;
use axum::{
    extract::{Multipart, State},
    Json,
};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetBookToUpstreamRequest {
    pub book_id: String,
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
    /// 正文字数（字符数）。展示用——字节数对中文正文没有可读性。
    pub chars: i64,
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
    // 字数不在版本表里存，逐条从正文池读出来数（默认 50 条、面板按需打开，
    // zstd 解压是微秒级）；blob 缺失属数据损坏，直接报错而不是默默显示 0。
    let mut items: Vec<ChapterTimelineItem> = Vec::with_capacity(rows.len());
    for row in &rows {
        let chars = state
            .chapter_content_service
            .load_content(row.content_id)
            .await?
            .chars()
            .count() as i64;
        items.push(ChapterTimelineItem {
            version_id: row.version_id,
            kind: row.kind.clone(),
            created_at: row.created_at,
            raw_size: row.raw_size,
            chars,
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
        });
    }
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
    let chars = view.content.chars().count() as i64;
    Ok(Json(ApiResponse::ok(json!({
        "versionId": view.version_id,
        "kind": view.kind,
        "baseVersionId": view.base_version_id,
        "createdAt": view.created_at,
        "rawSize": view.raw_size,
        "chars": chars,
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
        .chapter_diff_service
        .diff_texts(&from.content, &to.content)
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

/// 整书换回最新上游：清掉本书全部版本选择与用户版本指针，每章显示
/// 回落到最新上游（本地书回到原文）。历史版本严格保留，可从正文历史找回。
pub async fn reset_book_to_upstream(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ResetBookToUpstreamRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if req.book_id.trim().is_empty() {
        return Err(AppError::BadRequest("bookId required".to_string()));
    }
    resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    let cleared = state
        .chapter_content_service
        .reset_book_to_upstream(&user.0.ns, &req.book_id)
        .await?;
    Ok(Json(ApiResponse::ok(json!({ "cleared": cleared }))))
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

// ── 单章 / 整本 TXT 导入 ──────────────────────────────────────────────

#[derive(Debug, Default)]
struct ChapterImportRequest {
    book_id: String,
    mode: ImportMode,
    chapter_url: String,
    chapter_index: i32,
    chapter_title: String,
    kind: String,
    note: String,
    make_current: bool,
    created_at: Option<i64>,
    /// 为空表示「全选已匹配章节」；这个字段本身不出现时也是全选。
    selected_indexes: Option<Vec<i32>>,
    text: String,
    file_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChapterImportPreviewItem {
    source_index: i32,
    title: String,
    chars: usize,
    excerpt: String,
    /// matched / empty / unmatched
    status: &'static str,
    /// title / number / order / single / none
    match_kind: &'static str,
    chapter_index: Option<i32>,
    chapter_url: Option<String>,
    chapter_title: Option<String>,
    /// 当前是否有缓存正文可用来对比。
    cached: bool,
    /// 导入正文与当前缓存正文是否不同；没有缓存时为 false。
    changed: bool,
}

/// 预览时把解析后的正文暂存在进程内，点开某章看 diff 时不必重传整本 TXT。
struct CachedImportChapter {
    source_index: i32,
    content: String,
    chapter_url: Option<String>,
}

struct CachedImportPreview {
    user_ns: String,
    book_id: String,
    created_at: Instant,
    chapters: Vec<CachedImportChapter>,
}

/// 预览缓存只为「点开某章看 diff」服务，限制存活时间、条数和总字节数。
const IMPORT_PREVIEW_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_IMPORT_PREVIEW_ENTRIES: usize = 3;
const MAX_IMPORT_PREVIEW_BYTES: usize = 24 * 1024 * 1024;

static IMPORT_PREVIEWS: Lazy<Mutex<HashMap<String, CachedImportPreview>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn store_import_preview(
    user_ns: &str,
    book_id: &str,
    items: &[ChapterImportPreviewItem],
    contents: Vec<String>,
) -> Option<String> {
    let total_bytes: usize = contents.iter().map(String::len).sum();
    if total_bytes > MAX_IMPORT_PREVIEW_BYTES {
        return None;
    }
    let mut cache = IMPORT_PREVIEWS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let now = Instant::now();
    cache.retain(|_, entry| now.duration_since(entry.created_at) < IMPORT_PREVIEW_TTL);
    while cache.len() >= MAX_IMPORT_PREVIEW_ENTRIES {
        let oldest = cache
            .iter()
            .min_by_key(|(_, entry)| entry.created_at)
            .map(|(id, _)| id.clone());
        match oldest {
            Some(id) => {
                cache.remove(&id);
            }
            None => break,
        }
    }
    let preview_id = crate::util::crypto::random_string(32);
    cache.insert(
        preview_id.clone(),
        CachedImportPreview {
            user_ns: user_ns.to_string(),
            book_id: book_id.to_string(),
            created_at: now,
            chapters: items
                .iter()
                .zip(contents)
                .map(|(item, content)| CachedImportChapter {
                    source_index: item.source_index,
                    content,
                    chapter_url: item.chapter_url.clone(),
                })
                .collect(),
        },
    );
    Some(preview_id)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterImportDiffQuery {
    pub preview_id: String,
    pub source_index: i32,
}

async fn read_chapter_import_request(
    mut multipart: Multipart,
) -> Result<ChapterImportRequest, AppError> {
    let mut req = ChapterImportRequest {
        kind: "edited".to_string(),
        ..ChapterImportRequest::default()
    };
    let mut count = 0usize;
    while let Some(field) = next_field_limited(&mut multipart, &mut count).await? {
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" {
            req.file_bytes = Some(
                read_limited_multipart_field(field, MAX_TXT_UPLOAD_BYTES, "TXT 文件不能超过 50MB")
                    .await?,
            );
            continue;
        }
        // 粘贴的整本正文也允许到 50MB，其余字段只放小参数。
        let limit = if name == "text" {
            MAX_TXT_UPLOAD_BYTES
        } else {
            8 * 1024
        };
        let bytes = read_limited_multipart_field(field, limit, "导入参数过长").await?;
        let value = String::from_utf8_lossy(&bytes).trim().to_string();
        match name.as_str() {
            "text" => req.text = value,
            "bookId" => req.book_id = value,
            "mode" => req.mode = ImportMode::parse(&value),
            "chapterUrl" => req.chapter_url = value,
            "chapterIndex" => {
                req.chapter_index = value
                    .parse()
                    .map_err(|_| AppError::BadRequest("chapterIndex 无效".to_string()))?
            }
            "chapterTitle" => req.chapter_title = value,
            "kind" => req.kind = value,
            "note" => req.note = value,
            "makeCurrent" => req.make_current = parse_bool(&value),
            "createdAt" => {
                if !value.is_empty() {
                    req.created_at = Some(
                        value
                            .parse()
                            .map_err(|_| AppError::BadRequest("createdAt 无效".to_string()))?,
                    );
                }
            }
            "selectedIndexes" => {
                let mut list = Vec::new();
                if !value.is_empty() {
                    for part in value.split(',') {
                        list.push(part.trim().parse::<i32>().map_err(|_| {
                            AppError::BadRequest("selectedIndexes 无效".to_string())
                        })?);
                    }
                }
                req.selected_indexes = Some(list);
            }
            _ => {}
        }
    }
    Ok(req)
}

fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn request_text(req: &ChapterImportRequest) -> Result<String, AppError> {
    let text = match req.file_bytes.as_ref() {
        Some(bytes) => decode_txt_bytes(bytes),
        None => req.text.clone(),
    };
    if text.trim().is_empty() {
        return Err(AppError::BadRequest("TXT 内容不能为空".to_string()));
    }
    Ok(text)
}

async fn load_book_chapters(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    resolved: &super::book::ResolvedBook,
) -> Result<Vec<BookChapter>, AppError> {
    if is_local_txt_url(&resolved.book_url) {
        return state
            .local_txt_book_service
            .get_chapter_list(user_ns, &resolved.book_url)
            .await;
    }
    if is_local_epub_url(&resolved.book_url) {
        return state
            .local_epub_book_service
            .get_chapter_list(user_ns, &resolved.book_url)
            .await;
    }
    let toc_url = match resolved
        .toc_url
        .clone()
        .filter(|url| !url.trim().is_empty())
    {
        Some(url) => url,
        None => state
            .book_service
            .get_book_info(
                user_ns,
                book_id,
                &resolved.source,
                &resolved.book_url,
                false,
            )
            .await?
            .toc_url
            .unwrap_or_else(|| resolved.book_url.clone()),
    };
    let toc_url = repair_encoded_url(&toc_url);
    state
        .book_service
        .get_chapter_list(user_ns, book_id, &resolved.source, &toc_url)
        .await
}

fn excerpt_of(content: &str) -> String {
    let mut out = String::new();
    for line in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(3)
    {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        if out.chars().count() >= 200 {
            break;
        }
    }
    out.chars().take(200).collect()
}

fn preview_from_match(
    matched: &MatchedChapter,
    toc: &[BookChapter],
    current_hash: Option<&String>,
) -> ChapterImportPreviewItem {
    let target = matched.toc_pos.map(|pos| &toc[pos]);
    let status = if matched.source.content.trim().is_empty() {
        "empty"
    } else if target.is_some() {
        "matched"
    } else {
        "unmatched"
    };
    let cached = current_hash.is_some();
    let changed = current_hash
        .map(|hash| *hash != normalized_content_hash(&matched.source.content))
        .unwrap_or(false);
    ChapterImportPreviewItem {
        source_index: matched.source.source_index,
        title: matched.source.title.clone(),
        chars: matched.source.content.chars().count(),
        excerpt: excerpt_of(&matched.source.content),
        status,
        match_kind: matched.match_kind.as_str(),
        chapter_index: target.map(|chapter| chapter.index),
        chapter_url: target.map(|chapter| chapter.url.clone()),
        chapter_title: target.map(|chapter| chapter.title.clone()),
        cached,
        changed,
    }
}

fn preview_response(
    mode: ImportMode,
    items: &[ChapterImportPreviewItem],
    preview_id: Option<String>,
) -> Value {
    let matched = items.iter().filter(|item| item.status == "matched").count();
    let empty = items.iter().filter(|item| item.status == "empty").count();
    let changed = items
        .iter()
        .filter(|item| item.status == "matched" && item.changed)
        .count();
    let cached = items
        .iter()
        .filter(|item| item.status == "matched" && item.cached)
        .count();
    json!({
        "mode": mode.as_str(),
        "total": items.len(),
        "matched": matched,
        "unmatched": items.len() - matched - empty,
        "empty": empty,
        "changed": changed,
        "cached": cached,
        "uncached": matched.saturating_sub(cached),
        "previewId": preview_id,
        "chapters": items,
    })
}

fn readable_error(error: &AppError) -> String {
    match error {
        AppError::BadRequest(message)
        | AppError::NotFound(message)
        | AppError::Unauthorized(message)
        | AppError::Blocked(message)
        | AppError::Conflict(message) => message.clone(),
        AppError::Upstream { message, .. } => message.clone(),
        AppError::Internal(_) => "服务端内部错误".to_string(),
        AppError::Db(_) => "数据库错误".to_string(),
        AppError::Http(_) => "网络错误".to_string(),
    }
}

async fn handle_chapter_import(
    state: AppState,
    user: CurrentUser,
    multipart: Multipart,
    commit: bool,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let req = read_chapter_import_request(multipart).await?;
    if req.book_id.trim().is_empty() {
        return Err(AppError::BadRequest("bookId required".to_string()));
    }
    if !matches!(req.kind.as_str(), "edited" | "imported") {
        return Err(AppError::BadRequest(
            "版本类型只能是 edited 或 imported".to_string(),
        ));
    }
    if req.note.len() > 2000 {
        return Err(AppError::BadRequest(
            "版本备注过长（最多 2000 字节）".to_string(),
        ));
    }
    let resolved = resolve_book_checked(&state, &user.0.ns, &req.book_id).await?;
    let text = request_text(&req)?;
    let parsed = split_text(&text, req.mode, &req.chapter_title);
    if parsed.len() > MAX_IMPORT_CHAPTERS {
        return Err(AppError::BadRequest(format!(
            "一次最多导入 {MAX_IMPORT_CHAPTERS} 章，当前文本解析出 {} 章",
            parsed.len()
        )));
    }
    let mode = match req.mode {
        ImportMode::Auto if parsed.len() > 1 => ImportMode::Book,
        ImportMode::Auto => ImportMode::Single,
        other => other,
    };

    // 预览才需要和当前版本比对；导入按前端勾选结果执行，不多查一遍。
    let effective: HashMap<String, String> = if commit {
        HashMap::new()
    } else {
        state
            .chapter_content_service
            .effective_normalized_hashes(&user.0.ns, &req.book_id)
            .await?
    };

    let mut items: Vec<ChapterImportPreviewItem> = Vec::new();
    let mut contents: Vec<String> = Vec::new();
    if mode == ImportMode::Single {
        if req.chapter_url.trim().is_empty() {
            return Err(AppError::BadRequest("单章导入需要 chapterUrl".to_string()));
        }
        let Some(source) = parsed.into_iter().next() else {
            return Err(AppError::BadRequest("TXT 内容不能为空".to_string()));
        };
        let title = if source.title.trim().is_empty() {
            req.chapter_title.clone()
        } else {
            source.title.clone()
        };
        let current = effective.get(&req.chapter_url);
        items.push(ChapterImportPreviewItem {
            source_index: source.source_index,
            title: title.clone(),
            chars: source.content.chars().count(),
            excerpt: excerpt_of(&source.content),
            status: if source.content.trim().is_empty() {
                "empty"
            } else {
                "matched"
            },
            match_kind: "single",
            chapter_index: Some(req.chapter_index),
            chapter_url: Some(req.chapter_url.clone()),
            chapter_title: Some(title),
            cached: current.is_some(),
            changed: current
                .map(|hash| *hash != normalized_content_hash(&source.content))
                .unwrap_or(false),
        });
        contents.push(source.content);
    } else {
        let toc = load_book_chapters(&state, &user.0.ns, &req.book_id, &resolved).await?;
        let matched = match_chapters(&parsed, &toc);
        for item in &matched {
            let current = item
                .toc_pos
                .map(|pos| &toc[pos])
                .and_then(|chapter| effective.get(&chapter.url));
            items.push(preview_from_match(item, &toc, current));
        }
        contents.extend(matched.into_iter().map(|item| item.source.content));
    }

    if !commit {
        let preview_id = store_import_preview(&user.0.ns, &req.book_id, &items, contents);
        return Ok(Json(ApiResponse::ok(preview_response(
            mode, &items, preview_id,
        ))));
    }

    let selected: Option<HashSet<i32>> = req
        .selected_indexes
        .as_ref()
        .map(|indexes| indexes.iter().copied().collect());
    let mut selected_total = 0usize;
    let mut imported = 0usize;
    let mut unchanged = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut errors = Vec::new();
    for (item, content) in items.iter().zip(contents.iter()) {
        let picked = selected
            .as_ref()
            .is_none_or(|indexes| indexes.contains(&item.source_index));
        if !picked || item.status != "matched" {
            continue;
        }
        let Some(chapter_url) = item.chapter_url.as_deref() else {
            skipped += 1;
            continue;
        };
        selected_total += 1;
        let title = item.chapter_title.as_deref().unwrap_or("");
        match state
            .chapter_content_service
            .save_user_version(
                &user.0.ns,
                &req.book_id,
                chapter_url,
                &req.kind,
                content,
                None,
                item.chapter_index.unwrap_or(0),
                title,
                &resolved.book_url,
                &req.note,
                req.make_current,
                req.created_at,
            )
            .await
        {
            Ok(RecordOutcome::Appended { .. }) => imported += 1,
            Ok(RecordOutcome::Unchanged { .. }) => unchanged += 1,
            Err(error) => {
                failed += 1;
                if errors.len() < 10 {
                    errors.push(format!(
                        "{}：{}",
                        if title.is_empty() { &item.title } else { title },
                        readable_error(&error)
                    ));
                }
            }
        }
    }
    Ok(Json(ApiResponse::ok(json!({
        "mode": mode.as_str(),
        "selected": selected_total,
        "imported": imported,
        "unchanged": unchanged,
        "skipped": skipped,
        "failed": failed,
        "errors": errors,
    }))))
}

/// 预览单章 / 整本 TXT 的解析与匹配结果，不写库。
pub async fn preview_chapter_import(
    State(state): State<AppState>,
    user: CurrentUser,
    multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    handle_chapter_import(state, user, multipart, false).await
}

/// 按预览结果批量导入。`selectedIndexes` 为空表示导入全部已匹配章节。
pub async fn import_chapter_text(
    State(state): State<AppState>,
    user: CurrentUser,
    multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    handle_chapter_import(state, user, multipart, true).await
}

/// 预览里点开某一章时，按需算它和当前正文章节的差异。
///
/// 差异只在详情里用一次，整本预览不预先算（721 章逐个起 git 不现实）。
pub async fn get_chapter_import_diff(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<ChapterImportDiffQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let preview_id = req.preview_id.trim();
    if preview_id.is_empty() {
        return Err(AppError::BadRequest("previewId required".to_string()));
    }
    let (book_id, incoming, chapter_url) = {
        let mut cache = IMPORT_PREVIEWS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let now = Instant::now();
        cache.retain(|_, entry| now.duration_since(entry.created_at) < IMPORT_PREVIEW_TTL);
        let entry = cache
            .get(preview_id)
            .ok_or_else(|| AppError::NotFound("预览已过期，请重新解析".to_string()))?;
        if entry.user_ns != user.0.ns {
            return Err(AppError::NotFound("预览已过期，请重新解析".to_string()));
        }
        let chapter = entry
            .chapters
            .iter()
            .find(|chapter| chapter.source_index == req.source_index)
            .ok_or_else(|| AppError::BadRequest("章节不存在".to_string()))?;
        (
            entry.book_id.clone(),
            chapter.content.clone(),
            chapter.chapter_url.clone(),
        )
    };
    let import_chars = incoming.chars().count();
    let Some(chapter_url) = chapter_url else {
        return Ok(Json(ApiResponse::ok(json!({
            "changed": Value::Null,
            "ops": Value::Null,
            "diffError": "这一章没有匹配到目录，无法对比",
            "currentExcerpt": "",
            "currentChars": Value::Null,
            "importChars": import_chars,
        }))));
    };
    let Some(current) = state
        .chapter_content_service
        .resolve(&user.0.ns, &book_id, &chapter_url, false)
        .await?
    else {
        return Ok(Json(ApiResponse::ok(json!({
            "changed": Value::Null,
            "ops": Value::Null,
            "diffError": "本章还没有缓存正文，无法对比；建议先缓存全书",
            "currentExcerpt": "",
            "currentChars": Value::Null,
            "importChars": import_chars,
        }))));
    };
    let current_chars = current.content.chars().count();
    let current_excerpt = excerpt_of(&current.content);
    if normalized_content_hash(&current.content) == normalized_content_hash(&incoming) {
        return Ok(Json(ApiResponse::ok(json!({
            "changed": false,
            "ops": [],
            "diffError": "",
            "currentExcerpt": current_excerpt,
            "currentChars": current_chars,
            "importChars": import_chars,
        }))));
    }
    let (ops, diff_error) = match state
        .chapter_diff_service
        .diff_texts(&current.content, &incoming)
    {
        Ok(ops) => (Some(ops), String::new()),
        Err(reason) => (None, reason.to_string()),
    };
    Ok(Json(ApiResponse::ok(json!({
        "changed": true,
        "ops": ops,
        "diffError": diff_error,
        "currentExcerpt": current_excerpt,
        "currentChars": current_chars,
        "importChars": import_chars,
    }))))
}
