//! 章节编辑域（v1）：编辑保存/取消、版本时间线、单版本、diff、版本选择、
//! 章节改名（标题覆盖层）、整书回到上游。
//!
//! 身份是 `(bookId, index)`，index 经 `resolve_chapter_url` 解析出内部
//! chapterUrl（红线 6）。响应沿用 /reader3 的 typed 形状（版本号以
//! `cv_<n>` 线缆字符串下发，前端适配层转回数字），服务调用全部复用
//! `chapter_content_service` / `chapter_diff_service`。
//!
//! 与 /reader3 的差异：编辑保存的章节标题快照不再由客户端上报，改为
//! 从章节登记表解析（客户端给值会漂移）；diff 的 `against` 缺省对最新
//! 上游（契约 §8.4）。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use crate::storage::db::content_repo::ChapterVersionRow;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/books/{bookId}/chapters/{index}/content",
            put(save_chapter_content)
                .delete(delete_chapter_content)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::api::router::PROTECTED_BODY_LIMIT_BYTES,
                )),
        )
        .route(
            "/v1/books/{bookId}/chapters/{index}/content/edit",
            get(get_chapter_edit),
        )
        .route(
            "/v1/books/{bookId}/chapters/{index}/content:select",
            post(select_chapter_version),
        )
        .route(
            "/v1/books/{bookId}/chapters/{index}/content:import",
            post(import_chapter_version).layer(axum::extract::DefaultBodyLimit::max(
                crate::api::router::MAX_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/v1/books/{bookId}/chapters/{index}/versions",
            get(list_chapter_versions),
        )
        .route(
            "/v1/books/{bookId}/chapters/{index}/versions/{versionId}",
            get(get_chapter_version),
        )
        .route(
            "/v1/books/{bookId}/chapters/{index}/versions/{versionId}/diff",
            get(get_chapter_version_diff),
        )
        .route(
            "/v1/books/{bookId}/chapters/{index}/title",
            get(get_chapter_title).delete(restore_chapter_title),
        )
        .route("/v1/books/{bookId}/chapters/{index}", patch(rename_chapter))
        .route(
            "/v1/books/{bookId}/chapter-marks",
            get(get_chapter_version_marks),
        )
        .route(
            "/v1/books/{bookId}/content:reset",
            post(reset_book_to_upstream),
        )
}

fn cv_id(version_id: i64) -> String {
    format!("cv_{version_id}")
}

fn parse_cv_id(raw: &str) -> Result<i64, Problem> {
    raw.strip_prefix("cv_")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            Problem::new(ErrorCode::MalformedRequest).detail("versionId 必须是 cv_<n> 形式")
        })
}

async fn resolve_chapter_or_404(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    index: i32,
) -> Result<(String, String), Problem> {
    let resolved = crate::api::handlers::book::resolve_book(state, user_ns, book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;
    let chapter_url =
        super::content::resolve_chapter_url(state, user_ns, book_id, &resolved, index).await?;
    Ok((resolved.book_url, chapter_url))
}

/// 登记表的章节标题快照（编辑保存用）：目录未缓存时为空串，
/// 与 /reader3 由客户端上报相比不会漂移。
fn chapter_not_found() -> Problem {
    Problem::new(ErrorCode::ChapterNotFound).detail("章节不存在")
}

async fn registry_chapter_title(state: &AppState, book_id: &str, index: i32) -> String {
    state
        .book_service
        .resolve_chapter_by_index(book_id, index)
        .await
        .ok()
        .flatten()
        .map(|chapter| chapter.title)
        .unwrap_or_default()
}

// ── 编辑保存 / 取消 ────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditChapterRequest {
    pub content: String,
    /// 编辑时看到的上游版本（cv_<n>）；缺省取当前最新上游
    pub base_version_id: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterWriteResult {
    pub edited: bool,
    pub version_id: String,
}

/// 保存编辑（生成新用户版本，设为当前显示）。
#[utoipa::path(
    put,
    path = "/v1/books/{bookId}/chapters/{index}/content",
    tag = "books",
    operation_id = "saveChapterContent",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
    ),
    request_body = EditChapterRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "已保存", body = ChapterWriteResult),
        (status = 400, description = "baseVersionId 不属于本章", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍或章节不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn save_chapter_content(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
    V1Json(request): V1Json<EditChapterRequest>,
) -> Result<Json<ChapterWriteResult>, Problem> {
    let (book_url, chapter_url) = resolve_chapter_or_404(&state, &user.ns, &book_id, index).await?;
    let base_version_id = match request.base_version_id {
        Some(raw) => {
            let id = parse_cv_id(&raw)?;
            let exists = state
                .chapter_content_service
                .version(&book_id, &chapter_url, id)
                .await
                .map_err(Problem::from)?;
            if exists.is_none() {
                return Err(
                    Problem::new(ErrorCode::MalformedRequest).detail("baseVersionId 不属于本章")
                );
            }
            Some(id)
        }
        None => state
            .chapter_content_service
            .latest(&book_id, &chapter_url)
            .await
            .map_err(Problem::from)?
            .and_then(|row| row.latest_upstream_version_id),
    };
    let chapter_title = registry_chapter_title(&state, &book_id, index).await;
    let outcome = state
        .chapter_content_service
        .save_user_version(
            &user.ns,
            &book_id,
            &chapter_url,
            "edited",
            &request.content,
            base_version_id,
            index,
            &chapter_title,
            &book_url,
            request.note.as_deref().unwrap_or(""),
            true,
            None,
        )
        .await
        .map_err(Problem::from)?;
    let version_id = match outcome {
        crate::storage::db::content_repo::RecordOutcome::Unchanged { version_id } => version_id,
        crate::storage::db::content_repo::RecordOutcome::Appended { version_id, .. } => version_id,
    };
    Ok(Json(ChapterWriteResult {
        edited: true,
        version_id: cv_id(version_id),
    }))
}

/// 取消当前编辑（清用户版本指针，历史保留），显示回落到最新上游。
#[utoipa::path(
    delete,
    path = "/v1/books/{bookId}/chapters/{index}/content",
    tag = "books",
    operation_id = "deleteChapterContent",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已取消（幂等）"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍或章节不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_chapter_content(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
) -> Result<StatusCode, Problem> {
    let (_, chapter_url) = resolve_chapter_or_404(&state, &user.ns, &book_id, index).await?;
    state
        .chapter_content_service
        .clear_current_user_version(&user.ns, &book_id, &chapter_url)
        .await
        .map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 当前用户编辑版（编辑器预填用）；没有编辑版本时 404。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters/{index}/content/edit",
    tag = "books",
    operation_id = "getChapterEdit",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "当前编辑版正文"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "本章没有编辑版本", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_chapter_edit(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
) -> Result<Json<ChapterEditViewPayload>, Problem> {
    let (_, chapter_url) = resolve_chapter_or_404(&state, &user.ns, &book_id, index).await?;
    match state
        .chapter_content_service
        .latest_user_version(&book_id, &chapter_url)
        .await
        .map_err(Problem::from)?
    {
        Some(view) => Ok(Json(ChapterEditViewPayload {
            content: view.content,
            version_id: cv_id(view.version_id),
            base_version_id: view.base_version_id.map(cv_id),
            kind: view.kind,
        })),
        None => Err(Problem::new(ErrorCode::ChapterEditNotFound).detail("本章还没有编辑版本")),
    }
}

/// 当前用户编辑版（编辑器预填用）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterEditViewPayload {
    pub content: String,
    pub version_id: String,
    pub base_version_id: Option<String>,
    /// `upstream` / `edited` / `imported`
    pub kind: String,
}

// ── 版本时间线 / 单版本 / diff / 选择 ──────────────────────────────────

/// 版本时间线条目（元数据，不含正文）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterTimelineItem {
    pub version_id: String,
    /// `upstream` / `edited` / `imported`
    pub kind: String,
    /// 毫秒时间戳（源材料原样）
    pub created_at: i64,
    pub raw_size: i64,
    /// 正文字数（字符数）
    pub chars: i64,
    pub chapter_index: i32,
    pub chapter_title: String,
    pub note: String,
    pub book_source_id: Option<i64>,
    pub book_source_name: String,
    pub base_version_id: Option<String>,
    pub is_current: bool,
    pub is_latest_upstream: bool,
    pub is_latest_user: bool,
}

/// 某章的版本时间线（倒序）与最新/选择指针。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterTimelinePayload {
    pub items: Vec<ChapterTimelineItem>,
    pub latest_upstream_version_id: Option<String>,
    pub latest_user_version_id: Option<String>,
    pub selected_version_id: Option<String>,
    pub effective_version_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListVersionsQuery {
    /// 缺省 50
    pub limit: Option<i64>,
    /// 分页游标：只取该版本之前的更旧记录
    pub before_version_id: Option<i64>,
}

fn timeline_item(
    row: &ChapterVersionRow,
    effective: Option<i64>,
    latest: Option<&crate::storage::db::content_repo::ChapterLatestRow>,
) -> ChapterTimelineItem {
    ChapterTimelineItem {
        version_id: cv_id(row.version_id),
        kind: row.kind.clone(),
        created_at: row.created_at,
        raw_size: row.raw_size,
        chars: 0,
        chapter_index: row.chapter_index,
        chapter_title: row.chapter_title.clone(),
        note: row.note.clone(),
        book_source_id: row.book_source_id,
        book_source_name: row.book_source_name.clone(),
        base_version_id: row.base_version_id.map(cv_id),
        is_current: effective == Some(row.version_id),
        is_latest_upstream: latest.and_then(|latest| latest.latest_upstream_version_id)
            == Some(row.version_id),
        is_latest_user: latest.and_then(|latest| latest.latest_user_version_id)
            == Some(row.version_id),
    }
}

/// 版本时间线（倒序，元数据不含正文）。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters/{index}/versions",
    tag = "books",
    operation_id = "listChapterVersions",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
        ("limit" = Option<i64>, Query, description = "缺省 50"),
        ("beforeVersionId" = Option<i64>, Query, description = "只取该版本之前的更旧记录"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "版本时间线", body = ChapterTimelinePayload),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍或章节不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_chapter_versions(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
    Query(query): Query<ListVersionsQuery>,
) -> Result<Json<ChapterTimelinePayload>, Problem> {
    let (_, chapter_url) = resolve_chapter_or_404(&state, &user.ns, &book_id, index).await?;
    let latest = state
        .chapter_content_service
        .latest(&book_id, &chapter_url)
        .await
        .map_err(Problem::from)?;
    let selected = state
        .chapter_content_service
        .selected_version_id(&user.ns, &book_id, &chapter_url)
        .await
        .map_err(Problem::from)?;
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
            &book_id,
            &chapter_url,
            query.limit.unwrap_or(50),
            query.before_version_id,
        )
        .await
        .map_err(Problem::from)?;
    // 字数不在版本表里存，逐条从正文池读出来数（zstd 解压微秒级，默认 50 条）
    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        let chars = state
            .chapter_content_service
            .load_content(row.content_id)
            .await
            .map_err(Problem::from)?
            .chars()
            .count() as i64;
        let mut item = timeline_item(row, effective, latest.as_ref());
        item.chars = chars;
        items.push(item);
    }
    Ok(Json(ChapterTimelinePayload {
        items,
        latest_upstream_version_id: latest
            .as_ref()
            .and_then(|row| row.latest_upstream_version_id)
            .map(cv_id),
        latest_user_version_id: latest
            .as_ref()
            .and_then(|row| row.latest_user_version_id)
            .map(cv_id),
        selected_version_id: selected.map(cv_id),
        effective_version_id: effective.map(cv_id),
    }))
}

/// 单个版本的正文（预览用，不改变当前选择）。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters/{index}/versions/{versionId}",
    tag = "books",
    operation_id = "getChapterVersion",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
        ("versionId" = String, Path, description = "版本 id（cv_<n>）"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "版本正文"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍、章节或版本不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_chapter_version(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index, version_id)): Path<(String, i32, String)>,
) -> Result<Json<ChapterVersionDetailPayload>, Problem> {
    let numeric = parse_cv_id(&version_id)?;
    let (_, chapter_url) = resolve_chapter_or_404(&state, &user.ns, &book_id, index).await?;
    let view = state
        .chapter_content_service
        .version(&book_id, &chapter_url, numeric)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| Problem::new(ErrorCode::ChapterNotFound).detail("章节版本不存在"))?;
    Ok(Json(ChapterVersionDetailPayload {
        version_id: cv_id(view.version_id),
        kind: view.kind,
        base_version_id: view.base_version_id.map(cv_id),
        created_at: view.created_at,
        raw_size: view.raw_size,
        chars: view.content.chars().count() as i64,
        note: view.note,
        book_source_name: view.book_source_name,
        chapter_index: view.chapter_index,
        chapter_title: view.chapter_title,
        content: view.content,
    }))
}

/// 单个版本的正文与元数据。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterVersionDetailPayload {
    pub version_id: String,
    /// `upstream` / `edited` / `imported`
    pub kind: String,
    pub base_version_id: Option<String>,
    pub created_at: i64,
    pub raw_size: i64,
    pub chars: i64,
    pub note: String,
    pub book_source_name: String,
    pub chapter_index: i32,
    pub chapter_title: String,
    pub content: String,
}

/// 任意两个版本的 diff；`against` 缺省对最新上游。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters/{index}/versions/{versionId}/diff",
    tag = "books",
    operation_id = "getChapterVersionDiff",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
        ("versionId" = String, Path, description = "起始版本（cv_<n>）"),
        ("against" = Option<String>, Query, description = "目标版本（cv_<n>）；缺省对最新上游"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "diff 操作序列"),
        (status = 400, description = "diff 不可用", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "版本不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_chapter_version_diff(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index, version_id)): Path<(String, i32, String)>,
    Query(query): Query<DiffAgainstQuery>,
) -> Result<Json<ChapterDiffPayload>, Problem> {
    let from_id = parse_cv_id(&version_id)?;
    let (_, chapter_url) = resolve_chapter_or_404(&state, &user.ns, &book_id, index).await?;
    let to_id = match query.against {
        Some(raw) => parse_cv_id(&raw)?,
        None => state
            .chapter_content_service
            .latest(&book_id, &chapter_url)
            .await
            .map_err(Problem::from)?
            .and_then(|row| row.latest_upstream_version_id)
            .ok_or_else(|| Problem::new(ErrorCode::ChapterNotFound).detail("本章还没有上游正文"))?,
    };
    let from = state
        .chapter_content_service
        .version(&book_id, &chapter_url, from_id)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| Problem::new(ErrorCode::ChapterNotFound).detail("起始版本不存在"))?;
    let to = state
        .chapter_content_service
        .version(&book_id, &chapter_url, to_id)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| Problem::new(ErrorCode::ChapterNotFound).detail("目标版本不存在"))?;
    let ops = state
        .chapter_diff_service
        .diff_texts(&from.content, &to.content)
        .map_err(|reason| {
            Problem::new(ErrorCode::MalformedRequest).detail(format!("diff 不可用: {reason}"))
        })?;
    Ok(Json(ChapterDiffPayload {
        from_version_id: cv_id(from.version_id),
        to_version_id: cv_id(to.version_id),
        ops,
    }))
}

/// 两版本之间的 diff 操作序列。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterDiffPayload {
    pub from_version_id: String,
    pub to_version_id: String,
    pub ops: Vec<crate::service::chapter_diff_service::DiffOp>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffAgainstQuery {
    pub against: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectChapterVersionRequest {
    /// `null` = 跟随最新（删除手动选择）
    pub version_id: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterSelectionPayload {
    pub selected_version_id: Option<String>,
    pub effective_version_id: Option<String>,
}

/// 设置当前显示版本；`versionId: null` 表示跟随最新。
#[utoipa::path(
    post,
    path = "/v1/books/{bookId}/chapters/{index}/content:select",
    tag = "books",
    operation_id = "selectChapterVersion",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
    ),
    request_body = SelectChapterVersionRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "选择后的指针", body = ChapterSelectionPayload),
        (status = 400, description = "版本不属于本章", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍或章节不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn select_chapter_version(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
    V1Json(request): V1Json<SelectChapterVersionRequest>,
) -> Result<Json<ChapterSelectionPayload>, Problem> {
    let version_id = match request.version_id {
        Some(raw) => Some(parse_cv_id(&raw)?),
        None => None,
    };
    let (_, chapter_url) = resolve_chapter_or_404(&state, &user.ns, &book_id, index).await?;
    state
        .chapter_content_service
        .select_version(&user.ns, &book_id, &chapter_url, version_id)
        .await
        .map_err(Problem::from)?;
    let latest = state
        .chapter_content_service
        .latest(&book_id, &chapter_url)
        .await
        .map_err(Problem::from)?;
    let effective = version_id
        .or_else(|| latest.as_ref().and_then(|row| row.latest_user_version_id))
        .or_else(|| {
            latest
                .as_ref()
                .and_then(|row| row.latest_upstream_version_id)
        });
    Ok(Json(ChapterSelectionPayload {
        selected_version_id: version_id.map(cv_id),
        effective_version_id: effective.map(cv_id),
    }))
}

// ── 手动导入单章 ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportChapterRequest {
    pub content: String,
    /// 编辑时看到的上游版本（cv_<n>）
    pub base_version_id: Option<String>,
    pub note: Option<String>,
    /// 默认只进时间线，不改变当前显示
    pub make_current: Option<bool>,
    /// 手动补历史时可指定时间（毫秒）
    pub created_at: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterImportResult {
    pub version_id: String,
    pub became_current: bool,
}

/// 手动导入一个历史版本（默认只进时间线，不改变当前显示）。
#[utoipa::path(
    post,
    path = "/v1/books/{bookId}/chapters/{index}/content:import",
    tag = "books",
    operation_id = "importChapterVersion",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
    ),
    request_body = ImportChapterRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "导入结果", body = ChapterImportResult),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍或章节不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn import_chapter_version(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
    V1Json(request): V1Json<ImportChapterRequest>,
) -> Result<Json<ChapterImportResult>, Problem> {
    let (book_url, chapter_url) = resolve_chapter_or_404(&state, &user.ns, &book_id, index).await?;
    let base_version_id = match request.base_version_id {
        Some(raw) => {
            let id = parse_cv_id(&raw)?;
            let exists = state
                .chapter_content_service
                .version(&book_id, &chapter_url, id)
                .await
                .map_err(Problem::from)?;
            if exists.is_none() {
                return Err(
                    Problem::new(ErrorCode::MalformedRequest).detail("baseVersionId 不属于本章")
                );
            }
            Some(id)
        }
        None => None,
    };
    let chapter_title = registry_chapter_title(&state, &book_id, index).await;
    let outcome = state
        .chapter_content_service
        .save_user_version(
            &user.ns,
            &book_id,
            &chapter_url,
            "imported",
            &request.content,
            base_version_id,
            index,
            &chapter_title,
            &book_url,
            request.note.as_deref().unwrap_or(""),
            request.make_current.unwrap_or(false),
            request.created_at,
        )
        .await
        .map_err(Problem::from)?;
    let (version_id, became_current) = match outcome {
        crate::storage::db::content_repo::RecordOutcome::Unchanged { version_id } => {
            (version_id, false)
        }
        crate::storage::db::content_repo::RecordOutcome::Appended {
            version_id,
            became_current,
            ..
        } => (version_id, became_current),
    };
    Ok(Json(ChapterImportResult {
        version_id: cv_id(version_id),
        became_current,
    }))
}

// ── 章节改名（标题覆盖层） ────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterTitlePayload {
    /// false 表示本次实际是「恢复原名」
    pub renamed: bool,
    pub title: String,
    /// 目录原标题（目录未缓存时为空串）
    pub original_title: String,
    pub has_override: bool,
}

/// 当前章的标题覆盖状态（改名输入框初始态）。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters/{index}/title",
    tag = "books",
    operation_id = "getChapterTitle",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "覆盖状态"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_chapter_title(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
) -> Result<Json<ChapterTitlePayload>, Problem> {
    title_status(&state, &user.ns, &book_id, index).await
}

/// 改当前章节名（持久覆盖层；与目录原标题相同按恢复原名处理）。
#[utoipa::path(
    patch,
    path = "/v1/books/{bookId}/chapters/{index}",
    tag = "books",
    operation_id = "renameChapter",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
    ),
    request_body = RenameChapterRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "改名结果", body = ChapterTitlePayload),
        (status = 400, description = "标题过长", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn rename_chapter(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
    V1Json(request): V1Json<RenameChapterRequest>,
) -> Result<Json<ChapterTitlePayload>, Problem> {
    if request.title.trim().is_empty() {
        return restore_chapter_title_inner(&state, &user.ns, &book_id, index).await;
    }
    let (book_row_id, chapter_url, original_title) =
        title_context(&state, &user.ns, &book_id, index).await?;
    let trimmed = request.title.trim();
    if chapter_url.is_empty() {
        // 章节定位失败（登记表为空/竞态）：拒绝而不是写到空 URL 键上
        return Err(chapter_not_found());
    }
    if !original_title.is_empty() && trimmed == original_title {
        state
            .chapter_title_override_repo
            .delete(book_row_id, &chapter_url)
            .await
            .map_err(Problem::from)?;
        return Ok(Json(ChapterTitlePayload {
            renamed: false,
            title: original_title.clone(),
            original_title,
            has_override: false,
        }));
    }
    if trimmed.chars().count() > CHAPTER_TITLE_MAX_LEN {
        return Err(Problem::new(ErrorCode::MalformedRequest)
            .detail(format!("章节标题过长（上限 {CHAPTER_TITLE_MAX_LEN} 字符）")));
    }
    state
        .chapter_title_override_repo
        .upsert(
            book_row_id,
            &chapter_url,
            trimmed,
            crate::util::time::now_ts(),
        )
        .await
        .map_err(Problem::from)?;
    Ok(Json(ChapterTitlePayload {
        renamed: true,
        title: trimmed.to_string(),
        original_title,
        has_override: true,
    }))
}

/// 取消改名（恢复目录原标题）。
#[utoipa::path(
    delete,
    path = "/v1/books/{bookId}/chapters/{index}/title",
    tag = "books",
    operation_id = "restoreChapterTitle",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "恢复结果", body = ChapterTitlePayload),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn restore_chapter_title(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
) -> Result<Json<ChapterTitlePayload>, Problem> {
    restore_chapter_title_inner(&state, &user.ns, &book_id, index).await
}

/// 章节标题长度上限：目录标题的现实量级，表级防线在请求侧兜住。
const CHAPTER_TITLE_MAX_LEN: usize = 100;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameChapterRequest {
    pub title: String,
}

/// 改名/恢复的公共上下文：登记行 id、章节内部地址、目录原标题。
///
/// 按 index 在「本地服务目录（本地书）/ 章节登记表（网络书）」里定位章节，
/// 同时拿到内部地址与目录原标题；目录未缓存时两者为空串（不做网络抓取，
/// 与 /reader3 的 rename 查询同口径）。
async fn title_context(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    index: i32,
) -> Result<(i64, String, String), Problem> {
    use crate::service::local_epub_book::is_local_epub_url;
    use crate::service::local_txt_book::is_local_txt_url;

    let resolved = crate::api::handlers::book::resolve_book(state, user_ns, book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;
    let book_row_id = crate::storage::db::repo::parse_book_id(book_id)
        .ok_or_else(|| Problem::new(ErrorCode::BookNotFound).detail("书籍不存在"))?;
    // 不走 resolve_chapter_url（那会触发目录抓取）：改名查询不应有网络副作用
    let chapter = if is_local_txt_url(&resolved.book_url) {
        state
            .local_txt_book_service
            .get_chapter_list(user_ns, &resolved.book_url)
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|chapter| chapter.index == index)
    } else if is_local_epub_url(&resolved.book_url) {
        state
            .local_epub_book_service
            .get_chapter_list(user_ns, &resolved.book_url)
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|chapter| chapter.index == index)
    } else {
        state
            .book_service
            .load_chapter_list_cache(user_ns, book_id)
            .await
            .map_err(Problem::from)?
            .and_then(|chapters| chapters.into_iter().find(|chapter| chapter.index == index))
    };
    let (chapter_url, original_title) = chapter
        .map(|chapter| (chapter.url, chapter.title))
        .unwrap_or_default();
    Ok((book_row_id, chapter_url, original_title))
}

async fn title_status(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    index: i32,
) -> Result<Json<ChapterTitlePayload>, Problem> {
    let (book_row_id, chapter_url, original_title) =
        title_context(state, user_ns, book_id, index).await?;
    let override_title = if chapter_url.is_empty() {
        None
    } else {
        state
            .chapter_title_override_repo
            .get_map(book_row_id)
            .await
            .map_err(Problem::from)?
            .remove(&chapter_url)
    };
    Ok(Json(ChapterTitlePayload {
        renamed: false,
        title: override_title
            .clone()
            .unwrap_or_else(|| original_title.clone()),
        original_title,
        has_override: override_title.is_some(),
    }))
}

async fn restore_chapter_title_inner(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    index: i32,
) -> Result<Json<ChapterTitlePayload>, Problem> {
    let (book_row_id, chapter_url, original_title) =
        title_context(state, user_ns, book_id, index).await?;
    if !chapter_url.is_empty() {
        state
            .chapter_title_override_repo
            .delete(book_row_id, &chapter_url)
            .await
            .map_err(Problem::from)?;
    }
    Ok(Json(ChapterTitlePayload {
        renamed: false,
        title: original_title.clone(),
        original_title,
        has_override: false,
    }))
}

// ── 整书回上游 / 版本标记 ─────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResetBookResult {
    /// 摘掉用户版本指针的章节数
    pub cleared: u64,
}

/// 整书换回最新上游：清掉全部版本选择与用户版本指针（历史保留）。
#[utoipa::path(
    post,
    path = "/v1/books/{bookId}/content:reset",
    tag = "books",
    operation_id = "resetBookToUpstream",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "清除的指针数", body = ResetBookResult),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn reset_book_to_upstream(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
) -> Result<Json<ResetBookResult>, Problem> {
    crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;
    let cleared = state
        .chapter_content_service
        .reset_book_to_upstream(&user.ns, &book_id)
        .await
        .map_err(Problem::from)?;
    Ok(Json(ResetBookResult { cleared }))
}

/// 每章当前显示版本的标记：键是章节内部地址（过渡期例外，同 ChapterItem.url
/// ——阅读器目录按 url 查表）；只含有版本记录的章。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterVersionMarkEntry {
    /// `upstream` / `edited` / `imported`
    pub kind: String,
    pub note: String,
    pub created_at: i64,
    pub is_latest_upstream: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterVersionMarksPayload {
    pub marks: std::collections::BTreeMap<String, ChapterVersionMarkEntry>,
}

/// 每章当前显示版本的标记（目录版本标签用）；没有的章按「最新上游」处理。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapter-marks",
    tag = "books",
    operation_id = "getChapterVersionMarks",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "章节地址 → 标记", body = ChapterVersionMarksPayload),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_chapter_version_marks(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
) -> Result<Json<ChapterVersionMarksPayload>, Problem> {
    crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;
    let rows = state
        .chapter_content_service
        .list_version_marks(&user.ns, &book_id)
        .await
        .map_err(Problem::from)?;
    let mut marks = std::collections::BTreeMap::new();
    for row in rows {
        marks.insert(
            row.chapter_url,
            ChapterVersionMarkEntry {
                kind: row.kind,
                note: row.note,
                created_at: row.created_at,
                is_latest_upstream: row.is_latest_upstream,
            },
        );
    }
    Ok(Json(ChapterVersionMarksPayload { marks }))
}
