//! `/v1/tasks` + `POST /v1/searches` + `POST /v1/sse-tickets`：P2 任务子系统。
//!
//! 契约 §7：任务注册 → 事件流（Last-Event-ID 续传）→ 取消 → 结果分页。
//! D13 先只打通 `kind=search`，其余 kind 复用同一注册表/事件/结果机制，
//! 逐个接入工作器。
//!
//! 结果顺序是「到达顺序」（去重后追加），不是最终排序——任务运行中客户端
//! 可以用 `item` 事件增量渲染，结束后用 `GET /v1/tasks/{id}/result` 分页
//! 对账。结果集 10MB 上限，超出标 `truncated: true`。

use crate::api::etag::{if_none_match_matches, strong_etag};
use crate::api::problem::{ErrorCode, FieldIssue, Problem};
use crate::api::v1::{V1Json, V1Query, V1User};
use crate::api::AppState;
use crate::model::book_source::BookSource;
use crate::model::search::SearchBook;
use crate::service::task_service::{
    PrefetchSummary, SourceImportResult, SourceTestResult, TaskKind, TaskPage, TaskProgress,
    TaskResultPage, TaskResultPayload, TaskState, TaskView,
};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use utoipa::{IntoParams, ToSchema};

/// 单个搜索任务的书源并发（全局任务上限 8 时，最坏 8×8=64 条出站连接）。
const SEARCH_TASK_CONCURRENCY: usize = 8;
const DEFAULT_RESULT_LIMIT: u32 = 50;
const MAX_RESULT_LIMIT: u32 = 200;
const DEFAULT_TASK_LIST_LIMIT: u32 = 50;
const MAX_TASK_LIST_LIMIT: u32 = 200;
const DEFAULT_LIMIT_PER_SOURCE: u32 = 20;
const MAX_LIMIT_PER_SOURCE: u32 = 100;
const MAX_QUERY_CHARS: usize = 200;
const MAX_SOURCE_IDS: usize = 64;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/searches", post(create_search))
        .route("/v1/tasks", get(list_tasks))
        .route("/v1/tasks/{taskId}", get(get_task).delete(cancel_task))
        .route("/v1/tasks/{taskId}/result", get(get_task_result))
        .route("/v1/tasks/{taskId}/events", get(task_events))
        .route("/v1/sse-tickets", post(create_sse_ticket))
}

// ─── POST /v1/searches ──────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchCreateRequest {
    /// 搜索关键字（必填，≤200 字符）
    #[schema(required = true, nullable = false, max_length = 200)]
    pub q: Option<String>,
    /// 与 `groupId` 二选一；省略则搜索全部启用书源（最多 64 个）
    #[schema(max_items = 64)]
    pub source_ids: Option<Vec<String>>,
    /// 书源分组名（契约 §8.6 字段名是 `groupId`；值仍是字符串形式的分组名，
    /// 不是实体 id——Draft-2 §16.2 记偏差）。`group` 兼容旧调用方。
    #[serde(alias = "group")]
    pub group_id: Option<String>,
    /// 每个书源最多保留多少条结果（默认 20，1..=100）
    #[schema(minimum = 1, maximum = 100)]
    pub limit_per_source: Option<u32>,
    /// 去重模式；目前只支持 `name-author`
    pub dedupe: Option<String>,
}

/// 创建搜索任务：202 + `Location: /v1/tasks/{id}`。
#[utoipa::path(
    post,
    path = "/v1/searches",
    tag = "tasks",
    operation_id = "createSearch",
    request_body = SearchCreateRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "幂等键（≤200 字符）：弱网重试返回同一个任务")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 202, description = "任务已入队；`Location` 指向 `/v1/tasks/{id}`；同 Idempotency-Key 重放返回原任务", body = TaskView, headers(("Location" = String, description = "任务资源地址"))),
        (status = 400, description = "dedupe 取值不支持", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "sourceIds 里的书源不存在", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "sourceIds 里的书源已停用", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "参数校验失败（q/sourceIds/groupId/limitPerSource）", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "并发任务超限，带 Retry-After", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create_search(
    State(state): State<AppState>,
    V1User(user): V1User,
    headers: HeaderMap,
    V1Json(request): V1Json<SearchCreateRequest>,
) -> Result<
    (
        StatusCode,
        [(header::HeaderName, String); 1],
        Json<TaskView>,
    ),
    Problem,
> {
    // 幂等重放：同一个 key 直接返回原任务（哪怕这次 body 不同）
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(key) = idempotency_key {
        if key.chars().count() > 200 {
            return Err(malformed("Idempotency-Key 不能超过 200 个字符"));
        }
        if let Some(task) = state.task_service.find_idempotent(&user.ns, key) {
            return Ok((
                StatusCode::ACCEPTED,
                [(header::LOCATION, format!("/v1/tasks/{}", task.id))],
                Json(task),
            ));
        }
    }

    let q = request.q.unwrap_or_default().trim().to_string();
    if q.is_empty() {
        return Err(field_error("/q", "REQUIRED", "搜索关键字不能为空"));
    }
    if q.chars().count() > MAX_QUERY_CHARS {
        return Err(field_error(
            "/q",
            "TOO_LONG",
            &format!("搜索关键字不能超过 {MAX_QUERY_CHARS} 个字符"),
        ));
    }
    let limit_per_source = request.limit_per_source.unwrap_or(DEFAULT_LIMIT_PER_SOURCE);
    if limit_per_source == 0 || limit_per_source > MAX_LIMIT_PER_SOURCE {
        return Err(field_error(
            "/limitPerSource",
            "OUT_OF_RANGE",
            &format!("limitPerSource 需在 1..={MAX_LIMIT_PER_SOURCE}"),
        ));
    }
    if request
        .source_ids
        .as_ref()
        .is_some_and(|ids| ids.len() > MAX_SOURCE_IDS)
    {
        return Err(field_error(
            "/sourceIds",
            "TOO_MANY",
            &format!("sourceIds 最多 {MAX_SOURCE_IDS} 个"),
        ));
    }
    if request.dedupe.as_deref().unwrap_or("name-author") != "name-author" {
        return Err(malformed("dedupe 目前只支持 name-author"));
    }
    let group = request
        .group_id
        .as_deref()
        .map(str::trim)
        .filter(|group| !group.is_empty());
    if request.source_ids.is_some() && group.is_some() {
        return Err(field_error(
            "/groupId",
            "MUTUALLY_EXCLUSIVE",
            "sourceIds 与 groupId 二选一",
        ));
    }

    // 配额先挡一刀：超限用户不必再为解析书源付出反序列化成本
    state
        .task_service
        .check_capacity(&user.ns, TaskKind::Search)?;
    let sources = resolve_search_sources(&state, &user.ns, request.source_ids, group).await?;
    if sources.is_empty() {
        let pointer = if group.is_some() {
            "/groupId"
        } else {
            "/sourceIds"
        };
        return Err(field_error(
            pointer,
            "REQUIRED",
            "没有可用的书源（检查书源是否已启用/分组是否为空）",
        ));
    }
    let (task, replayed) = state.task_service.create_with_idempotency(
        &user.ns,
        TaskKind::Search,
        TaskProgress::new(sources.len() as u32, "source"),
        idempotency_key,
    )?;
    if !replayed {
        spawn_search_worker(
            state.clone(),
            user.ns.clone(),
            task.id.clone(),
            q,
            sources,
            limit_per_source as usize,
        );
    }
    let location = format!("/v1/tasks/{}", task.id);
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(task),
    ))
}

async fn resolve_search_sources(
    state: &AppState,
    user_ns: &str,
    source_ids: Option<Vec<String>>,
    group: Option<&str>,
) -> Result<Vec<BookSource>, Problem> {
    if let Some(ids) = source_ids {
        if ids.is_empty() {
            return Err(field_error(
                "/sourceIds",
                "REQUIRED",
                "sourceIds 不能为空数组",
            ));
        }
        let mut seen = HashSet::new();
        let mut sources = Vec::new();
        for raw in ids {
            let id = super::sources::parse_source_path_id(&raw)?;
            if !seen.insert(id) {
                continue;
            }
            let source = state
                .book_source_service
                .get_by_id(user_ns, id)
                .await
                .map_err(Problem::from)?
                .ok_or_else(|| {
                    Problem::new(ErrorCode::SourceNotFound).detail(format!("书源不存在: {raw}"))
                })?;
            if source.enabled == Some(false) {
                return Err(Problem::new(ErrorCode::SourceDisabled)
                    .detail(format!("书源已停用: {}", source.book_source_name)));
            }
            sources.push(source);
        }
        return Ok(sources);
    }
    let mut list = state
        .book_source_service
        .list(user_ns)
        .await
        .map_err(Problem::from)?;
    // 与前端搜索口径一致：停用源不参与默认/分组搜索
    list.retain(|source| source.enabled != Some(false));
    if let Some(group) = group {
        // 分组名是逗号/分号/顿号分隔的字符串：按 split_source_groups 的权威
        // 口径拆分后精确相等，不能 `contains`（"玄幻" 会命中 "玄幻小说"）
        list.retain(|source| {
            source.book_source_group.as_deref().is_some_and(|groups| {
                crate::service::book_source_service::split_source_groups(groups)
                    .iter()
                    .any(|item| item == group)
            })
        });
    }
    Ok(list)
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

/// 400：语法/结构错误（未知枚举值等），见契约 §5.5 状态码表。
fn malformed(detail: &str) -> Problem {
    Problem::new(ErrorCode::MalformedRequest).detail(detail)
}

// ─── 搜索工作器（kind=search） ──────────────────────────────────────────

/// 单个书源搜索的并发任务返回值：书源（日志用）+ 搜索结果。
type SearchTaskResult = (
    BookSource,
    Result<Vec<SearchBook>, crate::error::error::AppError>,
);

/// 工作器兜底：panic/提前退出时把任务落到终态，不能让 `cancelling` 永久
/// 占着并发名额（watchdog 是第二道保险，见 `cancel_task`）。
struct WorkerGuard {
    service: Arc<crate::service::task_service::TaskService>,
    id: String,
    cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    done: bool,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let cancelled = self.cancel_flag.load(Ordering::SeqCst);
        let (state, error) = if cancelled {
            (TaskState::Cancelled, None)
        } else {
            (
                TaskState::Failed,
                Some(Problem::new(ErrorCode::Internal).detail("任务工作器异常退出")),
            )
        };
        self.service.finish(&self.id, state, error);
    }
}

fn spawn_search_worker(
    state: AppState,
    user_ns: String,
    task_id: String,
    q: String,
    sources: Vec<BookSource>,
    limit_per_source: usize,
) {
    let handle_registry = state.task_service.clone();
    let worker_task_id = task_id.clone();
    let worker = tokio::spawn(async move {
        let service = state.task_service.clone();
        service.mark_running(&task_id);
        let Some((cancel_flag, cancel_notify)) = service.cancellation(&task_id) else {
            return;
        };
        let mut guard = WorkerGuard {
            service: service.clone(),
            id: task_id.clone(),
            cancel_flag: cancel_flag.clone(),
            done: false,
        };
        let total = sources.len() as u32;
        let mut next = 0usize;
        let mut done = 0u32;
        let mut succeeded_sources = 0u32;
        let mut last_error: Option<Problem> = None;
        let semaphore = Arc::new(tokio::sync::Semaphore::new(SEARCH_TASK_CONCURRENCY));
        // JoinSet 在 Drop 时 abort 全部子任务：panic/提前 return 不会让在飞
        // 上游请求变成孤儿（JoinHandle 的 Drop 只是 detach）
        let mut in_flight: tokio::task::JoinSet<SearchTaskResult> = tokio::task::JoinSet::new();

        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                abort_all(&mut in_flight);
                service.finish(&task_id, TaskState::Cancelled, None);
                guard.done = true;
                return;
            }

            while in_flight.len() < SEARCH_TASK_CONCURRENCY && next < sources.len() {
                // 取消落在派发循环中间时，最多只多派一个请求；每轮 spawn 前
                // 再查一次，取消后不再发起新的上游请求
                if cancel_flag.load(Ordering::SeqCst) {
                    abort_all(&mut in_flight);
                    service.finish(&task_id, TaskState::Cancelled, None);
                    guard.done = true;
                    return;
                }
                let Ok(permit) = semaphore.clone().acquire_owned().await else {
                    break;
                };
                let source = sources[next].clone();
                let book_service = state.book_service.clone();
                let key = q.clone();
                let ns = user_ns.clone();
                let task_cancel = cancel_flag.clone();
                let outbound = state.task_service.outbound_semaphore();
                in_flight.spawn(async move {
                    let _permit = permit;
                    // 检查与 spawn 之间仍有微窗口：进任务后再查一次，
                    // DELETE 已置位时不发起新的上游请求
                    if task_cancel.load(Ordering::SeqCst) {
                        return (source, Ok(Vec::new()));
                    }
                    // 全局出站预算：多个任务叠加时限制总上游连接数
                    let Ok(_outbound_permit) = outbound.acquire_owned().await else {
                        return (source, Ok(Vec::new()));
                    };
                    let result = book_service.search_book(&ns, &source, &key, 1).await;
                    (source, result)
                });
                next += 1;
            }

            if in_flight.is_empty() {
                break;
            }

            // `notified()` 先创建再查标志位：取消通知不会落在两者之间被错过
            let notified = cancel_notify.notified();
            if cancel_flag.load(Ordering::SeqCst) {
                abort_all(&mut in_flight);
                service.finish(&task_id, TaskState::Cancelled, None);
                guard.done = true;
                return;
            }
            tokio::select! {
                _ = notified => {
                    abort_all(&mut in_flight);
                    service.finish(&task_id, TaskState::Cancelled, None);
                    guard.done = true;
                    return;
                }
                completed = in_flight.join_next() => {
                    match completed {
                        Some(Ok((_source, Ok(mut books)))) => {
                            succeeded_sources += 1;
                            if books.len() > limit_per_source {
                                books.truncate(limit_per_source);
                            }
                            // 封面收口是逐个 await 的收尾阶段：与取消通知竞争，
                            // 取消一落地立即 abort 其余书源，而不是等 I/O 收尾。
                            // 用块把 future 的借用限制在 append 之前。
                            {
                                let rewrite = state
                                    .image_service
                                    .rewrite_search_book_covers(&mut books);
                                tokio::pin!(rewrite);
                                let cover_notified = cancel_notify.notified();
                                if cancel_flag.load(Ordering::SeqCst) {
                                    abort_all(&mut in_flight);
                                    service.finish(&task_id, TaskState::Cancelled, None);
                                    guard.done = true;
                                    return;
                                }
                                tokio::select! {
                                    _ = cover_notified => {
                                        abort_all(&mut in_flight);
                                        service.finish(&task_id, TaskState::Cancelled, None);
                                        guard.done = true;
                                        return;
                                    }
                                    _ = &mut rewrite => {}
                                }
                            }
                            service.append_search_results(&task_id, books);
                        }
                        Some(Ok((source, Err(error)))) => {
                            tracing::warn!(
                                source = %source.book_source_name,
                                %error,
                                "任务搜索书源失败"
                            );
                            last_error = Some(Problem::from(error));
                        }
                        Some(Err(join)) => {
                            tracing::warn!(%join, "任务搜索 worker panic");
                        }
                        None => break,
                    }
                    done += 1;
                    service.emit_progress(
                        &task_id,
                        done,
                        total,
                        Some(format!("已完成 {done}/{total} 个书源")),
                    );
                }
            }
        }

        if succeeded_sources == 0 && !sources.is_empty() {
            let problem = last_error
                .map(|problem| {
                    if problem.code() == ErrorCode::MalformedRequest {
                        // 书源缺 searchUrl/规则不完整：不是调用方的请求错了，
                        // 别把 400 直接当成任务失败原因抛给客户端
                        Problem::new(ErrorCode::UpstreamFailed).detail(format!(
                            "书源规则不完整：{}",
                            problem.detail_text().unwrap_or("搜索失败")
                        ))
                    } else {
                        problem
                    }
                })
                .unwrap_or_else(|| {
                    Problem::new(ErrorCode::UpstreamFailed).detail("所有书源搜索失败")
                });
            service.finish(&task_id, TaskState::Failed, Some(problem));
        } else {
            service.finish(&task_id, TaskState::Succeeded, None);
        }
        guard.done = true;
    });
    handle_registry.register_worker(&worker_task_id, worker.abort_handle());
}

fn abort_all(tasks: &mut tokio::task::JoinSet<SearchTaskResult>) {
    // JoinSet::abort_all 会 abort 全部子任务；调用方随后立即 return，
    // 集合析构时不会再等待它们完成
    tasks.abort_all();
}

// ─── GET /v1/tasks ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListTasksQuery {
    /// 按状态过滤：queued/running/cancelling/succeeded/failed/cancelled
    pub state: Option<String>,
    /// 上一页的 nextCursor；空表示第一页
    pub cursor: Option<String>,
    /// 页大小，默认 50，上限 200
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
}

/// 我的任务列表（最新在前）。刷新页面后据此恢复还在跑的任务。
#[utoipa::path(
    get,
    path = "/v1/tasks",
    tag = "tasks",
    operation_id = "listTasks",
    params(ListTasksQuery),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "任务列表", body = TaskPage),
        (status = 400, description = "limit 越界或未知 state", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "游标无法解码", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_tasks(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Query(query): V1Query<ListTasksQuery>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let state_filter = match query.state.as_deref() {
        None => None,
        Some(raw) => Some(parse_task_state(raw)?),
    };
    let limit = query.limit.unwrap_or(DEFAULT_TASK_LIST_LIMIT);
    if limit == 0 || limit > MAX_TASK_LIST_LIMIT {
        return Err(malformed(&format!("limit 需在 1..={MAX_TASK_LIST_LIMIT}")));
    }
    let page = state
        .task_service
        .list(&user.ns, state_filter, query.cursor.as_deref(), limit)?;
    json_with_etag(&page, &headers)
}

fn parse_task_state(raw: &str) -> Result<TaskState, Problem> {
    match raw {
        "queued" => Ok(TaskState::Queued),
        "running" => Ok(TaskState::Running),
        "cancelling" => Ok(TaskState::Cancelling),
        "succeeded" => Ok(TaskState::Succeeded),
        "failed" => Ok(TaskState::Failed),
        "cancelled" => Ok(TaskState::Cancelled),
        other => Err(malformed(&format!("未知任务状态: {other}"))),
    }
}

// ─── GET/DELETE /v1/tasks/{taskId} ──────────────────────────────────────

/// 任务状态（事件流断开后先用它拿全量状态，再用 Last-Event-ID 补增量）。
#[utoipa::path(
    get,
    path = "/v1/tasks/{taskId}",
    tag = "tasks",
    operation_id = "getTask",
    params(("taskId" = String, Path, description = "任务 id（tsk_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "任务状态", body = TaskView),
        (status = 304, description = "If-None-Match 命中，任务状态未变"),
        (status = 404, description = "任务不存在或已过期", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_task(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let task = state
        .task_service
        .get(&user.ns, &task_id)
        .ok_or_else(task_not_found)?;
    json_with_etag(&task, &headers)
}

/// 请求取消（204；已结束的任务同样 204）。取消是「请求」：状态先变
/// `cancelling`，工作器察觉后落到 `cancelled`。5 秒 watchdog 兜底：
/// 工作器卡死/异常退出时强制落终态，不让名额被永久占用。
#[utoipa::path(
    delete,
    path = "/v1/tasks/{taskId}",
    tag = "tasks",
    operation_id = "cancelTask",
    params(("taskId" = String, Path, description = "任务 id（tsk_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已请求取消（幂等）"),
        (status = 404, description = "任务不存在或已过期", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn cancel_task(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(task_id): Path<String>,
) -> Result<StatusCode, Problem> {
    let Some((task, newly_requested)) = state.task_service.request_cancel(&user.ns, &task_id)
    else {
        return Err(task_not_found());
    };
    if newly_requested {
        // AI 资料任务的实际执行体是 sidecar 子进程：只置注册表取消位不够，
        // 工作器若卡住/未被调度，sidecar 会继续写资料。这里按书确认后
        // 直接终止当前 sidecar 任务（不会误杀另一本书）。
        if task.kind == TaskKind::AiGenerate {
            if let Some(book_id) = task.progress.subject_id.as_deref() {
                state
                    .agent_sidecar_service
                    .cancel_task_for_book(&user.ns, book_id)
                    .await;
            }
        }
        let service = state.task_service.clone();
        let id = task_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            service.force_cancel_timeout(&id);
        });
    }
    Ok(StatusCode::NO_CONTENT)
}

// ─── GET /v1/tasks/{taskId}/result ──────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct TaskResultQuery {
    /// 上一页的 nextCursor；空表示第一页
    pub cursor: Option<String>,
    /// 页大小，默认 50，上限 200
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
}

/// 任务结果：search 是结果分页，其余 kind 是各自的摘要。
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum TaskResultResponse {
    Search(TaskResultPage),
    Prefetch(PrefetchSummary),
    SourceTest(SourceTestResult),
    SourceImport(SourceImportResult),
}

/// 任务结果（按 kind 解释；search 去重后分页，prefetch-cache 返回摘要）。
#[utoipa::path(
    get,
    path = "/v1/tasks/{taskId}/result",
    tag = "tasks",
    operation_id = "getTaskResult",
    params(
        ("taskId" = String, Path, description = "任务 id（tsk_<n>）"),
        TaskResultQuery,
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "任务结果（search：TaskResultPage；prefetch-cache：PrefetchSummary）", body = TaskResultResponse),
        (status = 304, description = "If-None-Match 命中，结果页未变"),
        (status = 400, description = "limit 越界", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "任务不存在或已过期", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "结果游标无效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_task_result(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(task_id): Path<String>,
    V1Query(query): V1Query<TaskResultQuery>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let limit = query.limit.unwrap_or(DEFAULT_RESULT_LIMIT);
    if limit == 0 || limit > MAX_RESULT_LIMIT {
        return Err(malformed(&format!("limit 需在 1..={MAX_RESULT_LIMIT}")));
    }
    let payload = state
        .task_service
        .result_payload(&user.ns, &task_id, query.cursor.as_deref(), limit)
        .ok_or_else(task_not_found)??;
    let response = match payload {
        TaskResultPayload::Search(page) => TaskResultResponse::Search(page),
        TaskResultPayload::Prefetch(summary) => TaskResultResponse::Prefetch(summary),
        TaskResultPayload::SourceTest(summary) => TaskResultResponse::SourceTest(summary),
        TaskResultPayload::SourceImport(summary) => TaskResultResponse::SourceImport(summary),
    };
    json_with_etag(&response, &headers)
}

// ─── GET /v1/tasks/{taskId}/events（SSE） ───────────────────────────────

/// 任务事件流（text/event-stream）。
///
/// 事件名固定四种：`progress` / `item` / `error` / `done`，每条都有 `id:`；
/// 断线重连时带 `Last-Event-ID` 从缓冲重放。非浏览器客户端可先
/// `POST /v1/sse-tickets` 再以 `?ticket=` 连接（60 秒、一次性、绑定本路径）。
#[utoipa::path(
    get,
    path = "/v1/tasks/{taskId}/events",
    tag = "tasks",
    operation_id = "taskEvents",
    params(
        ("taskId" = String, Path, description = "任务 id（tsk_<n>）"),
        ("ticket" = Option<String>, Query, description = "SSE 票据（非浏览器客户端/离线导出；60 秒一次性）"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "SSE 事件流（text/event-stream）。`progress` 数据体为 TaskProgress，`item` 为 TaskItemEvent，`error` 为 Problem，`done` 为 TaskDoneEvent。", content_type = "text/event-stream", body = String),
        (status = 401, description = "票无效/过期，或未登录", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "任务不存在或已过期", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "同一任务的事件流订阅者过多", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn task_events(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, Problem> {
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let subscription = state
        .task_service
        .subscribe(&user.ns, &task_id, last_event_id)?;
    if subscription.gap {
        tracing::warn!(
            task_id,
            last_event_id,
            "Last-Event-ID 早于事件环起点，只重放保留部分"
        );
    }

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(32);
    let task_id_for_stream = task_id.clone();
    tokio::spawn(async move {
        for event in subscription.replay {
            if tx
                .send(Ok(to_sse_event(event.id, &event.event, &event.data)))
                .await
                .is_err()
            {
                return;
            }
        }
        // `done` 是任务最后一条事件，环形缓冲只会淘汰更早的条目：终态任务
        // 的 replay 已经包含它（客户端若已收到 done 并带相同 Last-Event-ID
        // 重连，replay 为空，直接关流即可，绝不补发重复 done）
        if subscription.terminal {
            return;
        }

        let mut receiver = subscription.receiver;
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let is_done = event.event == "done";
                    if tx
                        .send(Ok(to_sse_event(event.id, &event.event, &event.data)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if is_done {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    // 关流让 EventSource 带 Last-Event-ID 自动重连：subscribe
                    // 会从环形缓冲把丢掉的事件补齐（比在流内静默跳过快照安全）
                    tracing::warn!(task_id = %task_id_for_stream, skipped, "SSE 事件订阅落后，断开重连补齐");
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

fn to_sse_event(id: u64, name: &str, data: &str) -> Event {
    Event::default().id(id.to_string()).event(name).data(data)
}

// ─── POST /v1/sse-tickets ───────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SseTicketRequest {
    /// 要访问的事件流路径，如 `/v1/tasks/tsk_1/events`
    #[schema(required = true, nullable = false, max_length = 256)]
    pub path: Option<String>,
}

#[derive(Debug, serde::Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SseTicketResponse {
    pub ticket: String,
    #[schema(format = DateTime)]
    pub expires_at: String,
}

/// 签发一次性 SSE 票据（60 秒有效、绑定单个事件流路径）。
#[utoipa::path(
    post,
    path = "/v1/sse-tickets",
    tag = "tasks",
    operation_id = "createSseTicket",
    request_body = SseTicketRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 201, description = "已签发（响应 `Cache-Control: no-store`）", body = SseTicketResponse),
        (status = 422, description = "path 缺失或不是事件流路径", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "未消费票据过多，带 Retry-After", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create_sse_ticket(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<SseTicketRequest>,
) -> Result<Response, Problem> {
    let path = request.path.as_deref().unwrap_or_default().trim();
    if !is_task_events_path(path) {
        return Err(field_error(
            "/path",
            "INVALID_FORMAT",
            "path 必须是 /v1/tasks/{taskId}/events",
        ));
    }
    if path.len() > 256 {
        return Err(field_error("/path", "TOO_LONG", "path 不能超过 256 个字符"));
    }
    let (ticket, expires_epoch) = state.sse_ticket_service.issue(&user, path)?;
    let mut response = (
        StatusCode::CREATED,
        Json(SseTicketResponse {
            ticket,
            expires_at: crate::util::time::rfc3339_secs(expires_epoch),
        }),
    )
        .into_response();
    // 票据是短时凭据：任何中间缓存都不许落盘
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// `?ticket=` 只对任务事件流开放（鉴权中间件与签发端点共用这一份口径）。
pub(crate) fn is_task_events_path(path: &str) -> bool {
    path.strip_prefix("/v1/tasks/")
        .and_then(|rest| rest.strip_suffix("/events"))
        .is_some_and(|task_id| !task_id.is_empty() && !task_id.contains('/'))
}

fn task_not_found() -> Problem {
    Problem::new(ErrorCode::TaskNotFound).detail("任务不存在或已过期")
}

fn json_with_etag<T: serde::Serialize>(
    value: &T,
    headers: &HeaderMap,
) -> Result<Response, Problem> {
    let body = serde_json::to_vec(value).map_err(|error| {
        tracing::error!(%error, "任务响应序列化失败");
        Problem::new(ErrorCode::Internal).detail("响应序列化失败")
    })?;
    let etag = strong_etag(&body);
    if if_none_match_matches(headers.get(header::IF_NONE_MATCH), &etag) {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response.headers_mut().insert(header::ETAG, etag);
        return Ok(response);
    }
    let mut response = ([(header::CONTENT_TYPE, "application/json")], body).into_response();
    response.headers_mut().insert(header::ETAG, etag);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}
