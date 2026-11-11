//! 整本 TXT 导入链（v1）：分片上传、预览、导入、逐章差异。
//!
//! 与 `/reader3` 共用同一套核心（`run_chapter_import` /
//! `run_import_chunk_upload` / `chapter_import_diff_payload`），差异只在：
//! - 身份是 `(bookId, index)` 路径参数，表单里的 `bookId` 由路径给定；
//! - 响应是 typed payload（线缆与 /reader3 逐字段一致）。
//!
//! 这些路由**不进 120s 常规组**：由 `v1::router` 在超时层之外 merge，
//! 子路由自带 900s 上传超时与 64MB 体限、鉴权与 CSRF（见 v1::router）。

pub(crate) use crate::api::handlers::chapter_edit::ChapterImportDiffPayload;
use crate::api::handlers::chapter_edit::{
    run_chapter_import, ChapterImportOutcomePayload, ChapterImportPayload,
    ChapterImportPreviewPayload,
};
use crate::api::handlers::chapter_import_upload::{run_import_chunk_upload, ChunkUploadPayload};
use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::V1User;
use crate::api::AppState;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

pub fn router(state: AppState, upload_timeout: std::time::Duration) -> Router<AppState> {
    Router::new()
        .route(
            "/v1/books/{bookId}/chapters:preview-import",
            post(preview_chapter_import),
        )
        .route(
            "/v1/books/{bookId}/chapters:import",
            post(import_chapter_text),
        )
        .route(
            "/v1/books/{bookId}/chapters:upload-import-chunk",
            post(upload_chapter_import_chunk),
        )
        .route(
            "/v1/books/{bookId}/chapters/import-diff",
            get(get_chapter_import_diff),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::api::v1::require_auth,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(axum::middleware::from_fn(crate::api::v1::require_csrf))
        .layer(axum::middleware::from_fn_with_state(
            (
                upload_timeout,
                crate::api::inbound_guard::TimeoutShape::Problem,
            ),
            crate::api::inbound_guard::timeout_middleware,
        ))
}

fn book_id_or_404(book_id: &str) -> Result<String, Problem> {
    if book_id.trim().is_empty() {
        return Err(Problem::new(ErrorCode::BookNotFound).detail("书籍不存在"));
    }
    Ok(book_id.to_string())
}

/// 预览单章 / 整本 TXT 的解析与匹配结果，不写库。
#[utoipa::path(
    post,
    path = "/v1/books/{bookId}/chapters:preview-import",
    tag = "books",
    operation_id = "previewChapterImport",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    request_body = ChapterImportPreviewPayload,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "解析与匹配结果", body = ChapterImportPreviewPayload),
        (status = 400, description = "预览过期 / 书籍不一致 / 文本非法", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "TXT 超过 50MB", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn preview_chapter_import(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    multipart: axum::extract::Multipart,
) -> Result<Json<ChapterImportPreviewPayload>, Problem> {
    let book_id = book_id_or_404(&book_id)?;
    let mut req = crate::api::handlers::chapter_edit::read_chapter_import_request(multipart)
        .await
        .map_err(Problem::from)?;
    req.book_id = book_id;
    match run_chapter_import(state, &user.ns, req, false)
        .await
        .map_err(Problem::from)?
    {
        ChapterImportPayload::Preview(payload) => Ok(Json(payload)),
        ChapterImportPayload::Outcome(_) => unreachable!("preview 分支不会返回导入结果"),
    }
}

/// 按预览结果批量导入。`selectedIndexes` 为空表示导入全部已匹配章节。
#[utoipa::path(
    post,
    path = "/v1/books/{bookId}/chapters:import",
    tag = "books",
    operation_id = "importChapterText",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    request_body = ChapterImportOutcomePayload,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "导入结果", body = ChapterImportOutcomePayload),
        (status = 400, description = "预览过期 / 书籍不一致 / 文本非法", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "TXT 超过 50MB", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn import_chapter_text(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    multipart: axum::extract::Multipart,
) -> Result<Json<ChapterImportOutcomePayload>, Problem> {
    let book_id = book_id_or_404(&book_id)?;
    let mut req = crate::api::handlers::chapter_edit::read_chapter_import_request(multipart)
        .await
        .map_err(Problem::from)?;
    req.book_id = book_id;
    match run_chapter_import(state, &user.ns, req, true)
        .await
        .map_err(Problem::from)?
    {
        ChapterImportPayload::Outcome(payload) => Ok(Json(payload)),
        ChapterImportPayload::Preview(_) => unreachable!("import 分支不会返回预览"),
    }
}

/// 整本 TXT 的自适应分片上传：每片几 MB 以内，绕开反代/WAF 的单请求体积
/// 上限；预览与导入凭 uploadId 复用拼好的文件。
#[utoipa::path(
    post,
    path = "/v1/books/{bookId}/chapters:upload-import-chunk",
    tag = "books",
    operation_id = "uploadChapterImportChunk",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    request_body = ChunkUploadPayload,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "分片接收结果", body = ChunkUploadPayload),
        (status = 400, description = "分片为空 / 超出文件大小 / TXT 超上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn upload_chapter_import_chunk(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    multipart: axum::extract::Multipart,
) -> Result<Json<ChunkUploadPayload>, Problem> {
    book_id_or_404(&book_id)?;
    let payload = run_import_chunk_upload(&state, &user.ns, multipart)
        .await
        .map_err(Problem::from)?;
    Ok(Json(payload))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportDiffQuery {
    pub preview_id: String,
    pub source_index: i32,
}

/// 预览里点开某一章时，按需算它和当前正文的差异。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters/import-diff",
    tag = "books",
    operation_id = "getChapterImportDiff",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("previewId" = String, Query, description = "预览令牌"),
        ("sourceIndex" = i32, Query, description = "TXT 切章后的下标"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "逐段差异", body = ChapterImportDiffPayload),
        (status = 400, description = "章节不存在", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "预览已过期", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_chapter_import_diff(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    Query(query): Query<ImportDiffQuery>,
) -> Result<Json<crate::api::handlers::chapter_edit::ChapterImportDiffPayload>, Problem> {
    book_id_or_404(&book_id)?;
    let payload = crate::api::handlers::chapter_edit::chapter_import_diff_payload(
        &state,
        &user.ns,
        &query.preview_id,
        query.source_index,
    )
    .await
    .map_err(Problem::from)?;
    Ok(Json(payload))
}
