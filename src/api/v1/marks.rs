//! `/v1/me/marks`：书签与划线（`kind` 区分 `bookmark` / `underline`）。
//!
//! 身份是 `(bookId, chapterIndex, chapterPos, kind)` 五元组（`user_ns` 隐含），
//! **没有自然 id**——契约审计里的 `DELETE /v1/me/marks/{id}` 落成
//! `:batchDelete` 单条复用（decisions 记偏差）。书签必须挂在已登记的书上：
//! `book_url` 冗余列是恢复校验的唯一依据，放空等于放过「id 复用时挂到新书」。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use crate::model::bookmark::Bookmark;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/marks", get(list_marks).post(save_mark))
        .route("/v1/me/marks:batch", post(save_marks_batch))
        .route("/v1/me/marks:batchDelete", post(delete_marks_batch))
}

/// 单批上限：WebDAV 恢复是唯一的大批量场景，书签条目小，5000 留足余量。
const MAX_MARKS_BATCH: usize = 5000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListMarksQuery {
    /// 缺省返回全部书签（书签页是全量读取后本地按书分组）
    pub book_id: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchSaveMarksResult {
    pub saved: u32,
    /// 指向未登记书籍的条目被跳过（没有归属信息，存进去只会在 id 复用时串书）
    pub skipped: u32,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteMarksResult {
    pub deleted: u32,
}

fn validate_mark_book_id(bookmark: &Bookmark) -> Result<(), Problem> {
    if bookmark.book_id.trim().is_empty() {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("bookId 不能为空"));
    }
    Ok(())
}

async fn resolve_book_url(
    state: &AppState,
    user_ns: &str,
    bookmark: &Bookmark,
) -> Result<String, Problem> {
    state
        .book_service
        .book_repo()
        .get(user_ns, &bookmark.book_id)
        .await
        .map_err(Problem::from)?
        .map(|row| row.book_url)
        .ok_or_else(|| Problem::new(ErrorCode::BookNotFound).detail("书籍不存在"))
}

/// 全部书签与划线（按保存顺序）；`?bookId=` 过滤单本。
#[utoipa::path(
    get,
    path = "/v1/me/marks",
    tag = "me",
    operation_id = "listMarks",
    params(("bookId" = Option<String>, Query, description = "按书籍过滤；缺省全部")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "书签列表", body = [Bookmark]),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_marks(
    State(state): State<AppState>,
    V1User(user): V1User,
    Query(query): Query<ListMarksQuery>,
) -> Result<Json<Vec<Bookmark>>, Problem> {
    let mut marks = state
        .bookmark_repo
        .list(&user.ns)
        .await
        .map_err(Problem::from)?;
    if let Some(book_id) = query
        .book_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        marks.retain(|mark| mark.book_id == book_id);
    }
    Ok(Json(marks))
}

/// 保存单个书签/划线（同身份覆盖）。
#[utoipa::path(
    post,
    path = "/v1/me/marks",
    tag = "me",
    operation_id = "saveMark",
    request_body = Bookmark,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已保存"),
        (status = 400, description = "bookId 缺失", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn save_mark(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(bookmark): V1Json<Bookmark>,
) -> Result<StatusCode, Problem> {
    validate_mark_book_id(&bookmark)?;
    let book_url = resolve_book_url(&state, &user.ns, &bookmark).await?;
    state
        .bookmark_repo
        .upsert(&user.ns, &bookmark, &book_url)
        .await
        .map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 批量保存（WebDAV 恢复）：单事务；指向未登记书籍的条目跳过并计数。
#[utoipa::path(
    post,
    path = "/v1/me/marks:batch",
    tag = "me",
    operation_id = "saveMarksBatch",
    request_body = [Bookmark],
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "保存/跳过条数", body = BatchSaveMarksResult),
        (status = 400, description = "批量为空或超上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn save_marks_batch(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(marks): V1Json<Vec<Bookmark>>,
) -> Result<Json<BatchSaveMarksResult>, Problem> {
    if marks.is_empty() || marks.len() > MAX_MARKS_BATCH {
        return Err(Problem::new(ErrorCode::MalformedRequest)
            .detail(format!("marks 需要提供 1 到 {MAX_MARKS_BATCH} 条")));
    }
    // 同书多条只查一次登记表
    let mut url_cache: std::collections::HashMap<String, Option<String>> = Default::default();
    let mut items: Vec<(Bookmark, String)> = Vec::with_capacity(marks.len());
    let mut skipped = 0u32;
    for bookmark in marks {
        if bookmark.book_id.trim().is_empty() {
            skipped += 1;
            continue;
        }
        let book_url = match url_cache.get(&bookmark.book_id) {
            Some(url) => url.clone(),
            None => {
                let url = state
                    .book_service
                    .book_repo()
                    .get(&user.ns, &bookmark.book_id)
                    .await
                    .map_err(Problem::from)?
                    .map(|row| row.book_url);
                url_cache.insert(bookmark.book_id.clone(), url.clone());
                url
            }
        };
        match book_url {
            Some(book_url) => items.push((bookmark, book_url)),
            None => skipped += 1,
        }
    }
    let saved = items.len() as u32;
    state
        .bookmark_repo
        .upsert_many(&user.ns, &items)
        .await
        .map_err(Problem::from)?;
    Ok(Json(BatchSaveMarksResult { saved, skipped }))
}

/// 批量删除（按身份五元组；幂等，不存在的忽略）。单个删除也走这里。
#[utoipa::path(
    post,
    path = "/v1/me/marks:batchDelete",
    tag = "me",
    operation_id = "deleteMarksBatch",
    request_body = [Bookmark],
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "删除条数", body = BatchDeleteMarksResult),
        (status = 400, description = "批量为空或超上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_marks_batch(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(marks): V1Json<Vec<Bookmark>>,
) -> Result<Json<BatchDeleteMarksResult>, Problem> {
    if marks.is_empty() || marks.len() > MAX_MARKS_BATCH {
        return Err(Problem::new(ErrorCode::MalformedRequest)
            .detail(format!("marks 需要提供 1 到 {MAX_MARKS_BATCH} 条")));
    }
    let mut deleted = 0u32;
    for bookmark in &marks {
        let removed = state
            .bookmark_repo
            .delete(
                &user.ns,
                &bookmark.book_id,
                bookmark.chapter_index,
                bookmark.chapter_pos,
                &bookmark.kind,
            )
            .await
            .map_err(Problem::from)?;
        if removed {
            deleted += 1;
        }
    }
    Ok(Json(BatchDeleteMarksResult { deleted }))
}
