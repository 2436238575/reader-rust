//! 书源任务：批量/单源测试（`sources:test`）与导入（`sources:import`）。
//!
//! 两者都是 202 + `Location` 的后台任务：测试逐源跑搜索/发现规则并可选写
//! 「失效」分组；导入逐条宽松迁移（`book_source_from_value`）后 upsert。
//! 结果摘要通过 `GET /v1/tasks/{id}/result` 按 kind 返回（`SourceTestResult`
//! / `SourceImportResult`）。
//!
//! 取消检查点在每次派发/每完成一条后；子任务用 `JoinSet`（析构 abort），
//! 上游请求受任务全局出站预算约束。

use crate::api::problem::{ErrorCode, FieldIssue, Problem};
use crate::api::v1::{V1Json, V1Query, V1User};
use crate::api::AppState;
use crate::model::book_source::{book_source_from_value, BookSource};
use crate::service::book_source_service::set_invalid_book_source_group;
use crate::service::task_service::{
    SourceImportError, SourceImportResult, SourceTestItem, SourceTestResult, TaskKind,
    TaskProgress, TaskState, TaskView,
};
use axum::extract::{FromRequest, Multipart, Path, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::task::JoinSet;
use utoipa::{IntoParams, ToSchema};

/// 单次测试/导入的条目上限（与批量 upsert 的 1000 同量级）。
const MAX_SOURCE_TASK_ITEMS: usize = 1000;
/// 测试任务的源内并发（搜索 + 发现两次请求，8 路已足够）。
const SOURCE_TEST_CONCURRENCY: usize = 8;
/// 导入 worker 的保存并发（本地 SQLite 写，4 路即可）。
const SOURCE_IMPORT_CONCURRENCY: usize = 4;
/// 书源导入文件上限（JSON 文本，32MB 远超正常体积）。
const MAX_SOURCE_FILE_BYTES: usize = 32 * 1024 * 1024;

type SourceTestOutcome = (
    BookSource,
    crate::service::book_service::BookSourceAvailability,
);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sources:test", post(start_test_sources))
        // `/v1/sources/{sourceId}:test` 的 `:test` 后缀由 `{sourceId}` 路由承接
        //（matchit 不允许参数与字面量同段），见 sources.rs 的 POST `{id}`。
        .route(
            "/v1/sources:import",
            post(import_sources).layer(axum::extract::DefaultBodyLimit::max(
                crate::api::router::SAVE_SOURCES_BODY_LIMIT_BYTES,
            )),
        )
}

// ─── sources:test ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceTestRequest {
    /// `src_<n>` 列表；省略表示测试全部书源（最多 1000 个）
    pub ids: Option<Vec<String>>,
    /// 测试搜索时使用的关键字；省略用书源自带 checkKeyWord
    pub keyword: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct SourceTestQuery {
    /// 是否把无效源写入「失效」分组；默认 false（测试是读操作）
    pub mark_invalid: Option<bool>,
}

/// 批量测试书源 → 202 Task。
#[utoipa::path(
    post,
    path = "/v1/sources:test",
    tag = "sources",
    operation_id = "testSources",
    params(SourceTestQuery),
    request_body = SourceTestRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 202, description = "任务已入队", body = TaskView, headers(("Location" = String, description = "任务资源地址"))),
        (status = 404, description = "ids 中的书源不存在", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "ids 超过上限或没有可测试的书源", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "并发任务超限，带 Retry-After", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn start_test_sources(
    State(state): State<AppState>,
    V1User(user): V1User,
    headers: HeaderMap,
    V1Query(query): V1Query<SourceTestQuery>,
    V1Json(request): V1Json<SourceTestRequest>,
) -> Result<
    (
        StatusCode,
        [(header::HeaderName, String); 1],
        Json<TaskView>,
    ),
    Problem,
> {
    start_source_test(
        &state,
        &user,
        &headers,
        request.ids,
        request.keyword,
        query.mark_invalid.unwrap_or(false),
    )
    .await
}

/// 测试单个书源 → 202 Task（`?markInvalid=false` 默认 false）。
#[utoipa::path(
    post,
    path = "/v1/sources/{sourceId}:test",
    tag = "sources",
    operation_id = "testSource",
    params(
        ("sourceId" = String, Path, description = "书源 id（src_<n>）"),
        SourceTestQuery,
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 202, description = "任务已入队", body = TaskView, headers(("Location" = String, description = "任务资源地址"))),
        (status = 404, description = "书源不存在", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "并发任务超限，带 Retry-After", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn start_test_source(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(source_id): Path<String>,
    headers: HeaderMap,
    V1Query(query): V1Query<SourceTestQuery>,
) -> Result<
    (
        StatusCode,
        [(header::HeaderName, String); 1],
        Json<TaskView>,
    ),
    Problem,
> {
    // 外部契约路径是 `/v1/sources/{sourceId}:test`；matchit 不允许参数与
    // 字面量同段，实际由 `{sourceId}` 路由承接，这里剥掉 `:test` 后缀
    let Some(source_id) = source_id.strip_suffix(":test") else {
        return Err(Problem::new(ErrorCode::NotFound).detail("接口不存在"));
    };
    start_source_test(
        &state,
        &user,
        &headers,
        Some(vec![source_id.to_string()]),
        None,
        query.mark_invalid.unwrap_or(false),
    )
    .await
}

async fn start_source_test(
    state: &AppState,
    user: &crate::auth::extractor::AuthUser,
    headers: &HeaderMap,
    ids: Option<Vec<String>>,
    keyword: Option<String>,
    mark_invalid: bool,
) -> Result<
    (
        StatusCode,
        [(header::HeaderName, String); 1],
        Json<TaskView>,
    ),
    Problem,
> {
    let ids = match ids {
        Some(ids) => Some(parse_source_ids(&ids)?),
        None => None,
    };
    state
        .task_service
        .check_capacity(&user.ns, TaskKind::SourceTest)?;
    let sources = if let Some(ids) = ids {
        if ids.is_empty() {
            return Err(field_error("/ids", "REQUIRED", "ids 不能为空数组"));
        }
        let mut sources = Vec::with_capacity(ids.len());
        for id in ids {
            let source = state
                .book_source_service
                .get_by_id(&user.ns, id)
                .await
                .map_err(Problem::from)?
                .ok_or_else(|| {
                    Problem::new(ErrorCode::SourceNotFound).detail(format!("书源不存在: src_{id}"))
                })?;
            sources.push(source);
        }
        sources
    } else {
        state
            .book_source_service
            .list(&user.ns)
            .await
            .map_err(Problem::from)?
    };
    if sources.is_empty() {
        return Err(field_error("/ids", "REQUIRED", "没有可测试的书源"));
    }
    if sources.len() > MAX_SOURCE_TASK_ITEMS {
        return Err(field_error(
            "/ids",
            "TOO_MANY",
            &format!("单次最多测试 {MAX_SOURCE_TASK_ITEMS} 个书源"),
        ));
    }
    let keyword = keyword
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let (task, replayed) = state.task_service.create_with_idempotency(
        &user.ns,
        TaskKind::SourceTest,
        TaskProgress::new(sources.len() as u32, "source"),
        idempotency_key(headers)?,
    )?;
    if !replayed {
        spawn_source_test_worker(
            state.clone(),
            user.ns.clone(),
            task.id.clone(),
            sources,
            keyword,
            mark_invalid,
        );
    }
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, format!("/v1/tasks/{}", task.id))],
        Json(task),
    ))
}

fn parse_source_ids(ids: &[String]) -> Result<Vec<i64>, Problem> {
    if ids.len() > MAX_SOURCE_TASK_ITEMS {
        return Err(field_error(
            "/ids",
            "TOO_MANY",
            &format!("ids 最多 {MAX_SOURCE_TASK_ITEMS} 个"),
        ));
    }
    ids.iter()
        .map(|raw| super::sources::parse_source_path_id(raw))
        .collect()
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

fn idempotency_key(headers: &HeaderMap) -> Result<Option<&str>, Problem> {
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(key) = key {
        if key.chars().count() > 200 {
            return Err(Problem::new(ErrorCode::MalformedRequest)
                .detail("Idempotency-Key 不能超过 200 个字符"));
        }
    }
    Ok(key)
}

struct SourceTaskGuard {
    service: Arc<crate::service::task_service::TaskService>,
    id: String,
    done: bool,
    message: &'static str,
}

impl Drop for SourceTaskGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        self.service.finish(
            &self.id,
            TaskState::Failed,
            Some(Problem::new(ErrorCode::Internal).detail(self.message)),
        );
    }
}

fn spawn_source_test_worker(
    state: AppState,
    user_ns: String,
    task_id: String,
    sources: Vec<BookSource>,
    keyword: Option<String>,
    mark_invalid: bool,
) {
    tokio::spawn(async move {
        let service = state.task_service.clone();
        service.mark_running(&task_id);
        let Some((cancel_flag, cancel_notify)) = service.cancellation(&task_id) else {
            return;
        };
        let mut guard = SourceTaskGuard {
            service: service.clone(),
            id: task_id.clone(),
            done: false,
            message: "书源测试工作器异常退出",
        };
        let total = sources.len() as u32;
        let mut done = 0u32;
        let mut results = Vec::with_capacity(sources.len());
        let semaphore = Arc::new(tokio::sync::Semaphore::new(SOURCE_TEST_CONCURRENCY));
        let mut in_flight: JoinSet<SourceTestOutcome> = JoinSet::new();
        let mut index = 0usize;

        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                in_flight.abort_all();
                service.finish(&task_id, TaskState::Cancelled, None);
                guard.done = true;
                return;
            }
            while in_flight.len() < SOURCE_TEST_CONCURRENCY && index < sources.len() {
                if cancel_flag.load(Ordering::SeqCst) {
                    in_flight.abort_all();
                    service.finish(&task_id, TaskState::Cancelled, None);
                    guard.done = true;
                    return;
                }
                let Ok(permit) = semaphore.clone().acquire_owned().await else {
                    break;
                };
                let source = sources[index].clone();
                index += 1;
                let book_service = state.book_service.clone();
                let ns = user_ns.clone();
                let key = keyword.clone();
                let outbound = state.task_service.outbound_semaphore();
                in_flight.spawn(async move {
                    let _permit = permit;
                    let Ok(_outbound_permit) = outbound.acquire_owned().await else {
                        return (
                            source,
                            crate::service::book_service::BookSourceAvailability {
                                book_source_url: String::new(),
                                book_source_name: String::new(),
                                valid: false,
                                search_ok: false,
                                explore_ok: false,
                                keyword: String::new(),
                                explore_url: None,
                                search_error: Some("任务已取消".to_string()),
                                explore_error: None,
                            },
                        );
                    };
                    let availability = book_service
                        .test_book_source_availability(&ns, &source, key.as_deref())
                        .await;
                    (source, availability)
                });
            }
            if in_flight.is_empty() {
                break;
            }
            let notified = cancel_notify.notified();
            if cancel_flag.load(Ordering::SeqCst) {
                in_flight.abort_all();
                service.finish(&task_id, TaskState::Cancelled, None);
                guard.done = true;
                return;
            }
            tokio::select! {
                _ = notified => {
                    in_flight.abort_all();
                    service.finish(&task_id, TaskState::Cancelled, None);
                    guard.done = true;
                    return;
                }
                completed = in_flight.join_next() => {
                    let (mut source, availability) = match completed {
                        Some(Ok(pair)) => pair,
                        Some(Err(join)) => {
                            tracing::warn!(%join, "书源测试子任务异常退出");
                            done += 1;
                            service.emit_progress(
                                &task_id,
                                done,
                                total,
                                Some(format!("已测试 {done}/{total} 个书源")),
                            );
                            continue;
                        }
                        None => break,
                    };
                    let changed = mark_invalid
                        && set_invalid_book_source_group(&mut source, !availability.valid);
                    if changed {
                        if let Err(error) = state
                            .book_source_service
                            .save(&user_ns, source.clone())
                            .await
                        {
                            tracing::warn!(%error, "写回书源失效分组失败");
                        }
                    }
                    results.push(SourceTestItem {
                        source_id: source
                            .book_source_id
                            .map(|id| format!("src_{id}"))
                            .unwrap_or_default(),
                        name: availability.book_source_name,
                        valid: availability.valid,
                        search_ok: availability.search_ok,
                        explore_ok: availability.explore_ok,
                        keyword: availability.keyword,
                        search_error: availability.search_error,
                        explore_error: availability.explore_error,
                        marked_invalid: changed && !availability.valid,
                        group: source.book_source_group,
                    });
                    done += 1;
                    service.emit_progress(
                        &task_id,
                        done,
                        total,
                        Some(format!("已测试 {done}/{total} 个书源")),
                    );
                }
            }
        }

        results.sort_by(|a, b| a.name.cmp(&b.name));
        let valid = results.iter().filter(|item| item.valid).count();
        let summary = SourceTestResult {
            total: results.len() as u32,
            valid: valid as u32,
            invalid: results.len().saturating_sub(valid) as u32,
            marked_invalid: results.iter().filter(|item| item.marked_invalid).count() as u32,
            results,
        };
        service.set_source_test_result(&task_id, summary);
        if done == 0 && total > 0 {
            service.finish(
                &task_id,
                TaskState::Failed,
                Some(Problem::new(ErrorCode::Internal).detail("书源测试全部异常退出")),
            );
        } else {
            service.finish(&task_id, TaskState::Succeeded, None);
        }
        guard.done = true;
    });
}

// ─── sources:import ─────────────────────────────────────────────────────

/// 仅用于 OpenAPI 的请求体形状；实际解析走原始 JSON（兼容数组/旧字段名）或多文件。
#[allow(dead_code)]
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceImportRequest {
    /// 源材料数组（宽松迁移，允许 legado 旧字段）
    pub sources: Vec<serde_json::Value>,
}

/// 导入书源（JSON `{sources: []}` 或 multipart JSON 文件）→ 202 Task。
#[utoipa::path(
    post,
    path = "/v1/sources:import",
    tag = "sources",
    operation_id = "importSources",
    request_body = SourceImportRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 202, description = "任务已入队", body = TaskView, headers(("Location" = String, description = "任务资源地址"))),
        (status = 400, description = "请求体不是合法的书源 JSON", body = Problem, content_type = "application/problem+json"),
        (status = 415, description = "Content-Type 必须是 application/json 或 multipart/form-data", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "sources 为空或超过上限", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "并发任务超限，带 Retry-After", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn import_sources(
    State(state): State<AppState>,
    V1User(user): V1User,
    request: Request,
) -> Result<
    (
        StatusCode,
        [(header::HeaderName, String); 1],
        Json<TaskView>,
    ),
    Problem,
> {
    let headers = request.headers().clone();
    state
        .task_service
        .check_capacity(&user.ns, TaskKind::SourceImport)?;
    let values = read_import_values(&state, request).await?;
    if values.is_empty() {
        return Err(field_error("/sources", "REQUIRED", "sources 不能为空"));
    }
    if values.len() > MAX_SOURCE_TASK_ITEMS {
        return Err(field_error(
            "/sources",
            "TOO_MANY",
            &format!("单次最多导入 {MAX_SOURCE_TASK_ITEMS} 个书源"),
        ));
    }
    let (task, replayed) = state.task_service.create_with_idempotency(
        &user.ns,
        TaskKind::SourceImport,
        TaskProgress::new(values.len() as u32, "source"),
        idempotency_key(&headers)?,
    )?;
    if !replayed {
        spawn_source_import_worker(state.clone(), user.ns.clone(), task.id.clone(), values);
    }
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, format!("/v1/tasks/{}", task.id))],
        Json(task),
    ))
}

async fn read_import_values(
    state: &AppState,
    request: Request,
) -> Result<Vec<serde_json::Value>, Problem> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if content_type.starts_with("application/json") {
        let bytes = axum::body::to_bytes(
            request.into_body(),
            crate::api::router::SAVE_SOURCES_BODY_LIMIT_BYTES,
        )
        .await
        .map_err(|_| Problem::new(ErrorCode::PayloadTooLarge).detail("导入材料超过 64MB"))?;
        let payload: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            Problem::new(ErrorCode::MalformedRequest).detail(format!("书源 JSON 不合法：{error}"))
        })?;
        return extract_source_values(payload);
    }
    if content_type.starts_with("multipart/form-data") {
        let mut multipart = Multipart::from_request(request, state)
            .await
            .map_err(|error| Problem::new(ErrorCode::MalformedRequest).detail(error.body_text()))?;
        let mut field_count = 0usize;
        while let Some(field) =
            crate::api::handlers::multipart::next_field_limited(&mut multipart, &mut field_count)
                .await
                .map_err(Problem::from)?
        {
            let is_json_file = field
                .file_name()
                .is_some_and(|name| name.ends_with(".json") || name.ends_with(".txt"));
            if !is_json_file {
                continue;
            }
            let bytes: Vec<u8> = crate::api::handlers::multipart::read_limited_multipart_field(
                field,
                MAX_SOURCE_FILE_BYTES,
                "书源文件过大（上限 32MB）",
            )
            .await
            .map_err(Problem::from)?;
            let payload: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
                Problem::new(ErrorCode::MalformedRequest)
                    .detail(format!("书源文件 JSON 不合法：{error}"))
            })?;
            return extract_source_values(payload);
        }
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("没有上传 JSON 书源文件"));
    }
    Err(Problem::new(ErrorCode::UnsupportedMediaType)
        .detail("Content-Type 必须是 application/json 或 multipart/form-data"))
}

fn extract_source_values(payload: serde_json::Value) -> Result<Vec<serde_json::Value>, Problem> {
    if let Some(items) = payload.as_array() {
        return Ok(items.clone());
    }
    if let Some(object) = payload.as_object() {
        for key in ["sources", "bookSources", "bookSourceList", "data"] {
            if let Some(items) = object.get(key).and_then(|value| value.as_array()) {
                return Ok(items.clone());
            }
        }
    }
    Err(Problem::new(ErrorCode::MalformedRequest).detail("书源材料必须是数组或 {sources: []}"))
}

fn spawn_source_import_worker(
    state: AppState,
    user_ns: String,
    task_id: String,
    values: Vec<serde_json::Value>,
) {
    tokio::spawn(async move {
        let service = state.task_service.clone();
        service.mark_running(&task_id);
        let Some((cancel_flag, cancel_notify)) = service.cancellation(&task_id) else {
            return;
        };
        let mut guard = SourceTaskGuard {
            service: service.clone(),
            id: task_id.clone(),
            done: false,
            message: "书源导入工作器异常退出",
        };
        let total = values.len() as u32;
        let mut done = 0u32;
        let mut summary = SourceImportResult {
            total,
            ..SourceImportResult::default()
        };
        let semaphore = Arc::new(tokio::sync::Semaphore::new(SOURCE_IMPORT_CONCURRENCY));
        let mut in_flight: JoinSet<Result<i64, (u32, String)>> = JoinSet::new();
        let mut index = 0usize;

        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                in_flight.abort_all();
                service.set_source_import_result(&task_id, summary);
                service.finish(&task_id, TaskState::Cancelled, None);
                guard.done = true;
                return;
            }
            while in_flight.len() < SOURCE_IMPORT_CONCURRENCY && index < values.len() {
                if cancel_flag.load(Ordering::SeqCst) {
                    in_flight.abort_all();
                    service.set_source_import_result(&task_id, summary);
                    service.finish(&task_id, TaskState::Cancelled, None);
                    guard.done = true;
                    return;
                }
                let value = values[index].clone();
                let item_index = index as u32;
                index += 1;
                let Ok(permit) = semaphore.clone().acquire_owned().await else {
                    break;
                };
                let source_service = state.book_source_service.clone();
                let ns = user_ns.clone();
                in_flight.spawn(async move {
                    let _permit = permit;
                    let source = book_source_from_value(value)
                        .map_err(|error| (item_index, format!("书源格式不合法：{error}")))?;
                    source_service
                        .save(&ns, source)
                        .await
                        .map_err(|error| (item_index, error.client_message().to_string()))
                });
            }
            if in_flight.is_empty() {
                break;
            }
            let notified = cancel_notify.notified();
            if cancel_flag.load(Ordering::SeqCst) {
                in_flight.abort_all();
                service.set_source_import_result(&task_id, summary);
                service.finish(&task_id, TaskState::Cancelled, None);
                guard.done = true;
                return;
            }
            tokio::select! {
                _ = notified => {
                    in_flight.abort_all();
                    service.set_source_import_result(&task_id, summary);
                    service.finish(&task_id, TaskState::Cancelled, None);
                    guard.done = true;
                    return;
                }
                completed = in_flight.join_next() => {
                    match completed {
                        Some(Ok(Ok(_id))) => summary.imported += 1,
                        Some(Ok(Err((item_index, error)))) => {
                            summary.failed += 1;
                            summary.errors.push(SourceImportError { index: item_index, error });
                        }
                        Some(Err(join)) => {
                            summary.failed += 1;
                            summary.errors.push(SourceImportError {
                                index: 0,
                                error: format!("导入子任务异常：{join}"),
                            });
                        }
                        None => break,
                    }
                    done += 1;
                    service.emit_progress(
                        &task_id,
                        done,
                        total,
                        Some(format!("已导入 {done}/{total} 个书源")),
                    );
                }
            }
        }

        summary.errors.sort_by_key(|item| item.index);
        let all_failed = summary.imported == 0 && summary.total > 0;
        service.set_source_import_result(&task_id, summary);
        if all_failed {
            service.finish(
                &task_id,
                TaskState::Failed,
                Some(Problem::new(ErrorCode::ValidationFailed).detail("全部书源导入失败")),
            );
        } else {
            service.finish(&task_id, TaskState::Succeeded, None);
        }
        guard.done = true;
    });
}
