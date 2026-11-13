//! `/v1/cache`：缓存的占用统计与显式清理（同步子集）。
//!
//! 契约 §8.6 把预下载（`POST /v1/books/{id}/cache` → 202 Task）与 policy
//! 清理（`{policy: olderThan}` → 202 Task）设计成任务模型——任务子系统落地
//! 前，本批先交付同步能力：
//! - `GET /v1/cache/stats`：各层占用（ETag/304）；
//! - `DELETE /v1/cache?kind=`：按层清理当前用户缓存，缺省清全部层；
//! - `DELETE /v1/books/{bookId}/cache`：书籍级清理（重置目录抓取状态 + 换源
//!   候选；登记行与正文历史不动，与 `/reader3/deleteBookCache` 同语义）。
//!
//! 「书籍级清理拆『重置目录/候选』与『清除已下载正文』两个动作」仍是待
//! 拍板项（见 decisions），拍板前不引入删除正文历史的入口。

use crate::api::problem::{ErrorCode, FieldIssue, Problem};
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use crate::model::book_source::BookSource;
use crate::service::book_service::{CacheKind, CachePurgeResult, CacheStatsSnapshot};
use crate::service::task_service::{PrefetchSummary, TaskKind, TaskProgress, TaskState, TaskView};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/cache/stats", get(cache_stats))
        .route("/v1/cache", delete(purge_cache))
        .route(
            "/v1/books/{bookId}/cache",
            post(start_prefetch).delete(delete_book_cache),
        )
}

/// 单层缓存清理的响应：各层被删除的文件数（正文历史恒 0——不可清理）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CachePurgeResponse {
    pub purged: CachePurgeResult,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachePurgeQuery {
    /// 缺省清理全部层；非法取值返回 400
    pub kind: Option<String>,
}

/// `cover` / `chapterList` / `searchResults` / `review`，与 `/reader3` 的
/// `CacheKind` 同名。手工解析（Query 的枚举反序列化失败是裸 400，
/// 不是 problem 形态）。
fn parse_kind(raw: Option<&str>) -> Result<Option<CacheKind>, Problem> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let kind = match raw {
        "cover" => CacheKind::Cover,
        "chapterList" => CacheKind::ChapterList,
        "searchResults" => CacheKind::SearchResults,
        "review" => CacheKind::Review,
        other => {
            return Err(Problem::new(ErrorCode::MalformedRequest).detail(format!(
                "kind 只接受 cover / chapterList / searchResults / review，收到 {other}"
            )))
        }
    };
    Ok(Some(kind))
}

/// 各层缓存的当前占用（当前用户视角；图片目录全局共享，计入本用户统计）。
#[utoipa::path(
    get,
    path = "/v1/cache/stats",
    tag = "cache",
    operation_id = "getCacheStats",
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "各层占用", body = CacheStatsSnapshot),
        (status = 304, description = "If-None-Match 命中，无 body"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn cache_stats(
    State(state): State<AppState>,
    V1User(user): V1User,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let snapshot = state
        .book_service
        .cache_stats(Some(&user.ns))
        .await
        .map_err(Problem::from)?;
    let body = serde_json::to_vec(&snapshot).expect("cache stats serializes");
    let etag = crate::api::etag::strong_etag(&body);
    let mut response =
        if crate::api::etag::if_none_match_matches(headers.get(header::IF_NONE_MATCH), &etag) {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            ([(header::CONTENT_TYPE, "application/json")], body).into_response()
        };
    response.headers_mut().insert(header::ETAG, etag);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}

/// 显式清理当前用户的缓存：`?kind=` 按层，缺省全部层（含全局共享的图片
/// 目录——单用户部署的「我的缓存」就是全部可再生缓存）。
#[utoipa::path(
    delete,
    path = "/v1/cache",
    tag = "cache",
    operation_id = "purgeCache",
    params(("kind" = Option<String>, Query, description = "cover / chapterList / searchResults / review；缺省全部层")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "各层被删除的文件数", body = CachePurgeResponse),
        (status = 400, description = "kind 取值非法", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn purge_cache(
    State(state): State<AppState>,
    V1User(user): V1User,
    Query(query): Query<CachePurgeQuery>,
) -> Result<Json<CachePurgeResponse>, Problem> {
    let kind = parse_kind(query.kind.as_deref())?;
    let purged = match kind {
        Some(kind) => {
            state
                .book_service
                .purge_user_cache_kind(&user.ns, kind)
                .await
        }
        None => state.book_service.purge_user_cache(&user.ns).await,
    }
    .map_err(Problem::from)?;
    Ok(Json(CachePurgeResponse { purged }))
}

/// 书籍级清理：重置目录抓取状态并删除换源候选；登记行、章节登记表与正文
/// 历史不动（离线阅读与重新入架都接得上）。幂等。
#[utoipa::path(
    delete,
    path = "/v1/books/{bookId}/cache",
    tag = "books",
    operation_id = "deleteBookCache",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已清理（幂等）"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_book_cache(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
) -> Result<StatusCode, Problem> {
    crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;
    state
        .book_service
        .reset_chapter_list_cache(&user.ns, &book_id)
        .await
        .map_err(Problem::from)?;
    let _ = state
        .book_service
        .delete_book_sources_cache(&user.ns, &book_id)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

// ─── POST /v1/books/{bookId}/cache：预下载任务 ─────────────────────────

/// 预下载范围：`from` 是章节下标（缺省 0），`count` 是章数（缺省 50，1..=1000）。
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrefetchRequest {
    #[schema(minimum = 0)]
    pub from: Option<u32>,
    #[schema(minimum = 1, maximum = 1000)]
    pub count: Option<u32>,
}

/// 预下载章节到正文池：202 + `Location: /v1/tasks/{id}`。
#[utoipa::path(
    post,
    path = "/v1/books/{bookId}/cache",
    tag = "cache",
    operation_id = "prefetchBookCache",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("Idempotency-Key" = Option<String>, Header, description = "幂等键（≤200 字符）"),
    ),
    request_body = PrefetchRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 202, description = "任务已入队", body = TaskView, headers(("Location" = String, description = "任务资源地址"))),
        (status = 400, description = "count 越界", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "本地书 / 目录为空 / from 越界", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "并发任务超限，带 Retry-After", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn start_prefetch(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    headers: HeaderMap,
    V1Json(request): V1Json<PrefetchRequest>,
) -> Result<
    (
        StatusCode,
        [(header::HeaderName, String); 1],
        Json<TaskView>,
    ),
    Problem,
> {
    use crate::service::local_epub_book::is_local_epub_url;
    use crate::service::local_txt_book::is_local_txt_url;

    let count = request.count.unwrap_or(50);
    if count == 0 || count > 1000 {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("count 需在 1..=1000"));
    }
    let from = request.from.unwrap_or(0) as usize;
    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;
    if is_local_txt_url(&resolved.book_url) || is_local_epub_url(&resolved.book_url) {
        return Err(field_error("/bookId", "INVALID_FORMAT", "本地书无需预下载"));
    }
    let toc_url = resolved
        .toc_url
        .clone()
        .unwrap_or_else(|| resolved.book_url.clone());
    let chapters = state
        .book_service
        .get_chapter_list(&user.ns, &book_id, &resolved.source, &toc_url)
        .await
        .map_err(Problem::from)?;
    if chapters.is_empty() {
        return Err(field_error("/bookId", "REQUIRED", "目录为空，无法预下载"));
    }
    if from >= chapters.len() {
        return Err(field_error(
            "/from",
            "OUT_OF_RANGE",
            &format!("from 超出目录范围（共 {} 章）", chapters.len()),
        ));
    }
    let end = (from + count as usize).min(chapters.len());
    let selected: Vec<(String, String)> = chapters[from..end]
        .iter()
        .map(|chapter| (chapter.url.clone(), chapter.title.clone()))
        .collect();

    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(key) = idempotency_key {
        if key.chars().count() > 200 {
            return Err(Problem::new(ErrorCode::MalformedRequest)
                .detail("Idempotency-Key 不能超过 200 个字符"));
        }
    }
    let (task, replayed) = state.task_service.create_with_idempotency(
        &user.ns,
        TaskKind::PrefetchCache,
        TaskProgress::new(selected.len() as u32, "chapter"),
        idempotency_key,
    )?;
    if !replayed {
        spawn_prefetch_worker(
            state.clone(),
            user.ns.clone(),
            task.id.clone(),
            book_id,
            resolved.source,
            selected,
        );
    }
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, format!("/v1/tasks/{}", task.id))],
        Json(task),
    ))
}

fn field_error(pointer: &str, code: &str, detail: &str) -> Problem {
    Problem::new(ErrorCode::ValidationFailed)
        .detail(detail)
        .errors(vec![FieldIssue {
            pointer: pointer.to_string(),
            code: code.to_string(),
            detail: Some(detail.to_string()),
        }])
}

struct PrefetchGuard {
    service: Arc<crate::service::task_service::TaskService>,
    id: String,
    done: bool,
}

impl Drop for PrefetchGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        self.service.finish(
            &self.id,
            TaskState::Failed,
            Some(Problem::new(ErrorCode::Internal).detail("预下载工作器异常退出")),
        );
    }
}

fn spawn_prefetch_worker(
    state: AppState,
    user_ns: String,
    task_id: String,
    book_id: String,
    source: BookSource,
    chapters: Vec<(String, String)>,
) {
    tokio::spawn(async move {
        let service = state.task_service.clone();
        service.mark_running(&task_id);
        let Some((cancel_flag, cancel_notify)) = service.cancellation(&task_id) else {
            return;
        };
        let mut guard = PrefetchGuard {
            service: service.clone(),
            id: task_id.clone(),
            done: false,
        };
        let total = chapters.len() as u32;
        let mut summary = PrefetchSummary {
            requested: total,
            ..PrefetchSummary::default()
        };
        let mut done = 0u32;
        let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
        let mut in_flight: tokio::task::JoinSet<Result<(), crate::error::error::AppError>> =
            tokio::task::JoinSet::new();
        let mut index = 0usize;

        loop {
            if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                in_flight.abort_all();
                service.set_prefetch_summary(&task_id, summary);
                service.finish(&task_id, TaskState::Cancelled, None);
                guard.done = true;
                return;
            }

            while in_flight.len() < 4 && index < chapters.len() {
                if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    in_flight.abort_all();
                    service.set_prefetch_summary(&task_id, summary);
                    service.finish(&task_id, TaskState::Cancelled, None);
                    guard.done = true;
                    return;
                }
                let (url, _title) = chapters[index].clone();
                index += 1;
                if state
                    .book_service
                    .is_chapter_cached(&user_ns, &book_id, &url)
                    .await
                {
                    summary.skipped += 1;
                    done += 1;
                    service.emit_progress(
                        &task_id,
                        done,
                        total,
                        Some(format!("已处理 {done}/{total} 章")),
                    );
                    continue;
                }
                let Ok(permit) = semaphore.clone().acquire_owned().await else {
                    break;
                };
                let book_service = state.book_service.clone();
                let source = source.clone();
                let ns = user_ns.clone();
                let b_id = book_id.clone();
                let outbound = state.task_service.outbound_semaphore();
                in_flight.spawn(async move {
                    let _permit = permit;
                    let Ok(_outbound_permit) = outbound.acquire_owned().await else {
                        return Ok(());
                    };
                    book_service
                        .cache_chapter(&ns, &b_id, &source, &url, false)
                        .await
                });
            }

            if in_flight.is_empty() {
                break;
            }

            let notified = cancel_notify.notified();
            if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                in_flight.abort_all();
                service.set_prefetch_summary(&task_id, summary);
                service.finish(&task_id, TaskState::Cancelled, None);
                guard.done = true;
                return;
            }
            tokio::select! {
                _ = notified => {
                    in_flight.abort_all();
                    service.set_prefetch_summary(&task_id, summary);
                    service.finish(&task_id, TaskState::Cancelled, None);
                    guard.done = true;
                    return;
                }
                completed = in_flight.join_next() => {
                    match completed {
                        Some(Ok(Ok(()))) => summary.cached += 1,
                        Some(Ok(Err(error))) => {
                            summary.failed += 1;
                            tracing::warn!(%error, "预下载章节失败");
                        }
                        Some(Err(join)) => {
                            summary.failed += 1;
                            tracing::warn!(%join, "预下载 worker panic");
                        }
                        None => break,
                    }
                    done += 1;
                    service.emit_progress(
                        &task_id,
                        done,
                        total,
                        Some(format!("已处理 {done}/{total} 章")),
                    );
                }
            }
        }

        service.set_prefetch_summary(&task_id, summary);
        if summary.cached == 0 && summary.skipped == 0 && total > 0 {
            service.finish(
                &task_id,
                TaskState::Failed,
                Some(Problem::new(ErrorCode::UpstreamFailed).detail("所有章节预下载失败")),
            );
        } else {
            service.finish(&task_id, TaskState::Succeeded, None);
        }
        guard.done = true;
    });
}
