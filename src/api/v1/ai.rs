//! `/v1` AI 域：书籍 AI 资料、模型可用状态、AI 资料编排任务与模型代理。
//!
//! - 资料读写按 `(用户, bookId)` 隔离，只要求书籍已登记（不要求仍在书架，
//!   移出书架后仍能读回/清理资料）。
//! - 编排任务走统一任务注册表（`TaskKind::AiGenerate`）：`POST` 返回 202
//!   `TaskView`，`GET /v1/tasks/{id}` 与事件流、取消复用 P2 的整套机制；
//!   `progress.subjectId` 是 bookId、`progress.phase` 是 sidecar 阶段。
//!   sidecar 仍是「单用户单任务」，本域只是把它的快照映射成任务进度。
//! - 代理端点成功时原样流式透传（不是 JSON 资源，因此不套资源 schema）；
//!   失败统一 problem+json，上游状态码进 `upstreamCode`。

use crate::api::handlers::{proxy_image_request, proxy_request, ProxyErrorStyle};
use crate::api::problem::{ErrorCode, FieldIssue, Problem};
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use crate::error::error::AppError;
use crate::model::ai_book::{
    AiBookCharacter, AiBookLocation, AiBookMap, AiBookMemory, AiBookNote, AiBookRelationship,
};
use crate::model::ai_proxy::{AiProxyImageRequest, AiProxyRequest};
use crate::model::book::Book;
use crate::service::agent_sidecar_service::AgentTaskKind;
use crate::service::task_service::{TaskKind, TaskProgress, TaskService, TaskState, TaskView};
use crate::storage::db::repo::canonical_book_id;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use utoipa::ToSchema;

/// 单条 AI 资料的序列化上限（与 `/reader3` 同口径）。
const MAX_AI_MEMORY_BYTES: usize = 1024 * 1024;
/// sidecar 快照轮询间隔（进程内读，成本可忽略）。
const AGENT_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// 取消 watchdog：与 `DELETE /v1/tasks/{id}` 同口径。
const AGENT_CANCEL_WATCHDOG_SECS: u64 = 5;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/ai/model-status", get(get_model_status))
        .route(
            "/v1/books/{bookId}/ai-memory",
            get(get_memory).put(put_memory).delete(delete_memory),
        )
        .route("/v1/books/{bookId}/agent-tasks", post(start_agent_task))
        .route(
            "/v1/books/{bookId}/agent-task",
            get(get_agent_task).delete(cancel_agent_task),
        )
}

/// AI 代理是长请求 + 最大 8MB 体：与 `/reader3` 代理同档，不进常规
/// 120s / 1MB 组。成功响应是流式透传，鉴权与 CSRF 在子路由自含。
pub fn proxy_router(
    state: AppState,
    upload_timeout: Duration,
    concurrency: Option<tower::limit::GlobalConcurrencyLimitLayer>,
) -> Router<AppState> {
    let routes = Router::new()
        .route("/v1/ai/proxy", post(proxy))
        .route("/v1/ai/proxy-image", post(proxy_image));
    let routes = match concurrency {
        Some(layer) => routes.layer(layer),
        None => routes,
    };
    routes
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::api::v1::require_auth,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(
            crate::api::router::PROTECTED_BODY_LIMIT_BYTES,
        ))
        .layer(axum::middleware::from_fn(crate::api::v1::require_csrf))
        .layer(axum::middleware::from_fn_with_state(
            (
                upload_timeout,
                crate::api::inbound_guard::TimeoutShape::Problem,
            ),
            crate::api::inbound_guard::timeout_middleware,
        ))
}

// ─── 模型可用状态 ──────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiModelStatus {
    /// 任一服务端模型可用（文本/图片/语音）
    pub can_use_server_model: bool,
    pub text_ready: bool,
    pub image_ready: bool,
    pub speech_ready: bool,
    /// AI 资料编排 sidecar 是否就绪（启动握手探测）
    pub agent_ready: bool,
}

fn endpoint_ready(enabled: bool, base_url: &str, model: &str) -> bool {
    enabled && !base_url.trim().is_empty() && !model.trim().is_empty()
}

/// 后端 AI 模型只回报「是否配置可用」的布尔状态。
///
/// 配置本体（地址/Key/模型名）只存在于服务端 env，从不下发到浏览器。
#[utoipa::path(
    get,
    path = "/v1/me/ai/model-status",
    tag = "ai",
    operation_id = "getAiModelStatus",
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "各类型模型是否可用", body = AiModelStatus),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_model_status(
    State(state): State<AppState>,
    _user: V1User,
) -> Result<Json<AiModelStatus>, Problem> {
    let config = state.ai_model_service.get().await.map_err(Problem::from)?;
    let text_ready = endpoint_ready(
        config.text.enabled,
        &config.text.base_url,
        &config.text.model,
    );
    let image_ready = endpoint_ready(
        config.image.enabled,
        &config.image.base_url,
        &config.image.model,
    );
    let speech_ready = endpoint_ready(
        config.speech.enabled,
        &config.speech.base_url,
        &config.speech.model,
    );
    Ok(Json(AiModelStatus {
        can_use_server_model: text_ready || image_ready || speech_ready,
        text_ready,
        image_ready,
        speech_ready,
        agent_ready: state.agent_sidecar_service.agent_ready(),
    }))
}

// ─── 书籍 AI 资料 ──────────────────────────────────────────────────────

async fn registered_book(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
) -> Result<crate::storage::db::repo::BookRow, Problem> {
    state
        .book_service
        .book_repo()
        .get(user_ns, book_id)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| Problem::new(ErrorCode::BookNotFound).detail("书籍不存在"))
}

async fn shelf_book(state: &AppState, user_ns: &str, book_id: &str) -> Result<Book, Problem> {
    if let Some(book) = state
        .book_service
        .get_shelf_book(user_ns, book_id)
        .await
        .map_err(Problem::from)?
    {
        return Ok(book);
    }
    registered_book(state, user_ns, book_id).await?;
    Err(Problem::new(ErrorCode::BookNotInShelf).detail("书籍未加入书架"))
}

/// 路径里的 bookId 一律先归一成 `book_<n>`：`book_007`/`book_+7` 是同一本书，
/// 任务登记与查询必须用同一个键，否则按别名提交的任务查不回来。
fn canonical_path_book_id(raw: &str) -> Result<String, Problem> {
    canonical_book_id(raw).ok_or_else(|| Problem::new(ErrorCode::BookNotFound).detail("书籍不存在"))
}

/// 读取书籍 AI 资料；尚未生成时 404 `AI_MEMORY_NOT_FOUND`。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/ai-memory",
    tag = "ai",
    operation_id = "getAiBookMemory",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "AI 资料", body = AiBookMemory),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在或尚未生成 AI 资料", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_memory(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
) -> Result<Json<AiBookMemory>, Problem> {
    let book_id = canonical_path_book_id(&book_id)?;
    registered_book(&state, &user.ns, &book_id).await?;
    let mut memory = state
        .ai_book_service
        .get(&user.ns, &book_id)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| Problem::new(ErrorCode::AiMemoryNotFound).detail("这本书还没有 AI 资料"))?;
    memory.book_id = Some(book_id);
    Ok(Json(memory))
}

/// PUT 请求体：字段与 `AiBookMemory` 一致，但拒绝未知字段（v1 契约）。
/// `bookId`/`bookUrl` 即使传入也会被路径与登记表覆盖。
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct AiBookMemoryWrite {
    pub book_id: Option<String>,
    pub book_url: String,
    pub book_name: Option<String>,
    pub author: Option<String>,
    pub enabled: bool,
    pub processed_chapter_index: Option<i32>,
    pub processed_chapter_title: Option<String>,
    pub updated_at: i64,
    pub summary: String,
    pub worldview: Vec<AiBookNote>,
    pub characters: Vec<AiBookCharacter>,
    pub relationships: Vec<AiBookRelationship>,
    pub locations: Vec<AiBookLocation>,
    pub map: Option<AiBookMap>,
    pub map_dirty: bool,
    pub last_error: Option<String>,
}

impl From<AiBookMemoryWrite> for AiBookMemory {
    fn from(value: AiBookMemoryWrite) -> Self {
        Self {
            book_id: value.book_id,
            book_url: value.book_url,
            book_name: value.book_name,
            author: value.author,
            enabled: value.enabled,
            processed_chapter_index: value.processed_chapter_index,
            processed_chapter_title: value.processed_chapter_title,
            updated_at: value.updated_at,
            summary: value.summary,
            worldview: value.worldview,
            characters: value.characters,
            relationships: value.relationships,
            locations: value.locations,
            map: value.map,
            map_dirty: value.map_dirty,
            last_error: value.last_error,
        }
    }
}

/// 保存（整体替换）书籍 AI 资料；`bookUrl`/书名/作者以登记表当前绑定为准。
#[utoipa::path(
    put,
    path = "/v1/books/{bookId}/ai-memory",
    tag = "ai",
    operation_id = "putAiBookMemory",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    request_body = AiBookMemoryWrite,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "保存后的 AI 资料", body = AiBookMemory),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "资料超过 1MB", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn put_memory(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    V1Json(write): V1Json<AiBookMemoryWrite>,
) -> Result<Json<AiBookMemory>, Problem> {
    let book_id = canonical_path_book_id(&book_id)?;
    let row = registered_book(&state, &user.ns, &book_id).await?;
    let mut memory: AiBookMemory = write.into();
    let serialized = serde_json::to_vec(&memory).map_err(|error| {
        Problem::new(ErrorCode::ValidationFailed).detail(format!("AI 资料序列化失败：{error}"))
    })?;
    if serialized.len() > MAX_AI_MEMORY_BYTES {
        return Err(Problem::new(ErrorCode::PayloadTooLarge).detail("AI 资料超出大小上限（1MB）"));
    }
    memory.book_id = Some(book_id.clone());
    memory.book_url = row.book_url;
    if memory.book_name.as_deref().unwrap_or("").trim().is_empty() {
        memory.book_name = Some(row.name);
    }
    if memory.author.as_deref().unwrap_or("").trim().is_empty() {
        memory.author = Some(row.author);
    }
    let saved = state
        .ai_book_service
        .save_for_book(&user.ns, &book_id, &memory.book_url.clone(), memory)
        .await
        .map_err(Problem::from)?;
    Ok(Json(saved))
}

/// 删除书籍 AI 资料（幂等）。
#[utoipa::path(
    delete,
    path = "/v1/books/{bookId}/ai-memory",
    tag = "ai",
    operation_id = "deleteAiBookMemory",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已删除（幂等）"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_memory(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
) -> Result<StatusCode, Problem> {
    let book_id = canonical_path_book_id(&book_id)?;
    registered_book(&state, &user.ns, &book_id).await?;
    state
        .ai_book_service
        .delete(&user.ns, &book_id)
        .await
        .map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── AI 资料编排任务 ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTaskKindRequest {
    UpdateToCurrent,
    RedrawMap,
}

impl From<AgentTaskKindRequest> for AgentTaskKind {
    fn from(kind: AgentTaskKindRequest) -> Self {
        match kind {
            AgentTaskKindRequest::UpdateToCurrent => Self::UpdateToCurrent,
            AgentTaskKindRequest::RedrawMap => Self::RedrawMap,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartAgentTaskRequest {
    /// 缺省 `update-to-current`
    pub kind: Option<AgentTaskKindRequest>,
    /// 目标章 index；缺省取书架阅读进度
    #[schema(minimum = 0)]
    pub target_chapter_index: Option<i32>,
}

/// 提交 AI 资料任务 → 202 `TaskView`（`Location: /v1/tasks/{id}`）。
///
/// sidecar 单用户单任务：已有任务进行中返回 409 `TASK_ALREADY_RUNNING`。
#[utoipa::path(
    post,
    path = "/v1/books/{bookId}/agent-tasks",
    tag = "ai",
    operation_id = "startAgentTask",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    request_body = StartAgentTaskRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 202, description = "任务已入队", body = TaskView, headers(("Location" = String, description = "任务资源地址"))),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "书籍未加入书架或已有 AI 任务进行中", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "targetChapterIndex 为负数", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "并发任务超限，带 Retry-After", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn start_agent_task(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    V1Json(request): V1Json<StartAgentTaskRequest>,
) -> Result<
    (
        StatusCode,
        [(header::HeaderName, String); 1],
        Json<TaskView>,
    ),
    Problem,
> {
    let canonical_book_id = canonical_path_book_id(&book_id)?;
    let mut book = shelf_book(&state, &user.ns, &canonical_book_id).await?;
    if request.target_chapter_index.is_some_and(|index| index < 0) {
        return Err(Problem::new(ErrorCode::ValidationFailed)
            .detail("targetChapterIndex 不能为负数")
            .errors(vec![FieldIssue {
                pointer: "/targetChapterIndex".to_string(),
                code: "OUT_OF_RANGE".to_string(),
                detail: Some("必须 ≥ 0".to_string()),
            }]));
    }
    book.book_id = Some(canonical_book_id.clone());
    let kind: AgentTaskKind = request
        .kind
        .unwrap_or(AgentTaskKindRequest::UpdateToCurrent)
        .into();

    // 先挡并发名额再启动 sidecar：否则 429 场景会白拉起一次模型任务。
    state
        .task_service
        .check_capacity(&user.ns, TaskKind::AiGenerate)?;

    // 先在 sidecar 原子占位再建注册表任务：并发双击时只有一方能拿到
    // 单任务槽，另一方在提交阶段就得到 409，而不是先 202 再异步失败。
    let job_id = state
        .agent_sidecar_service
        .start_task(&user.ns, book, kind, request.target_chapter_index)
        .await
        .map_err(agent_problem)?;

    let mut progress = TaskProgress::new(0, "chapter");
    progress.subject_id = Some(canonical_book_id.clone());
    progress.phase = Some("loading".to_string());
    progress.message = Some("准备任务...".to_string());
    let task =
        match state
            .task_service
            .create_with_progress(&user.ns, TaskKind::AiGenerate, progress)
        {
            Ok(task) => task,
            Err(problem) => {
                // 注册表登记失败（容量/并发竞态）：刚占位的 sidecar 任务必须
                // 立即停掉，避免留下没有取消入口的孤儿任务。
                state
                    .agent_sidecar_service
                    .cancel_task_for_book(&user.ns, &canonical_book_id)
                    .await;
                return Err(problem);
            }
        };
    spawn_agent_worker(state.clone(), user.ns.clone(), task.id.clone(), job_id);
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, format!("/v1/tasks/{}", task.id))],
        Json(task),
    ))
}

/// 这本书最近一次 AI 任务（含已结束；服务重启后任务随内存注册表消失）。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/agent-task",
    tag = "ai",
    operation_id = "getAgentTask",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "最近一次任务", body = TaskView),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在或该书还没有 AI 任务", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_agent_task(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
) -> Result<Json<TaskView>, Problem> {
    let book_id = canonical_path_book_id(&book_id)?;
    registered_book(&state, &user.ns, &book_id).await?;
    state
        .task_service
        .latest_for_subject(&user.ns, TaskKind::AiGenerate, &book_id)
        .map(Json)
        .ok_or_else(|| Problem::new(ErrorCode::TaskNotFound).detail("这本书还没有 AI 任务"))
}

/// 取消这本书正在跑的 AI 任务（幂等；已结束或无任务都返回 204）。
#[utoipa::path(
    delete,
    path = "/v1/books/{bookId}/agent-task",
    tag = "ai",
    operation_id = "cancelAgentTask",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已请求取消（幂等）"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn cancel_agent_task(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
) -> Result<StatusCode, Problem> {
    let book_id = canonical_path_book_id(&book_id)?;
    registered_book(&state, &user.ns, &book_id).await?;
    let Some(task) =
        state
            .task_service
            .latest_for_subject(&user.ns, TaskKind::AiGenerate, &book_id)
    else {
        return Ok(StatusCode::NO_CONTENT);
    };
    if task.state.is_terminal() {
        return Ok(StatusCode::NO_CONTENT);
    }
    // 先置注册表取消位，再杀 sidecar：反序会让 worker 在两步之间看到
    // 「sidecar 已停、无 lastError」，把用户取消误判成成功。
    let Some((_, newly_requested)) = state.task_service.request_cancel(&user.ns, &task.id) else {
        return Ok(StatusCode::NO_CONTENT);
    };
    if newly_requested {
        // sidecar 是全局单任务：只有它当前确实在跑这本书时才杀进程，
        // 避免取消一本「还在排队」的任务时误杀别的书。
        state
            .agent_sidecar_service
            .cancel_task_for_book(&user.ns, &book_id)
            .await;
        let service = state.task_service.clone();
        let id = task.id;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(AGENT_CANCEL_WATCHDOG_SECS)).await;
            service.force_cancel_timeout(&id);
        });
    }
    Ok(StatusCode::NO_CONTENT)
}

struct AgentTaskGuard {
    service: Arc<TaskService>,
    id: String,
    done: bool,
}

impl Drop for AgentTaskGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        self.service.finish(
            &self.id,
            TaskState::Failed,
            Some(Problem::new(ErrorCode::Internal).detail("AI 任务工作器异常退出")),
        );
    }
}

fn agent_problem(error: AppError) -> Problem {
    match error {
        AppError::Conflict(message) => Problem::new(ErrorCode::TaskAlreadyRunning).detail(message),
        AppError::BadRequest(message) => Problem::new(ErrorCode::ValidationFailed).detail(message),
        AppError::Unauthorized(message) => Problem::new(ErrorCode::AuthExpired).detail(message),
        other => Problem::from(other),
    }
}

fn chapter_count(index: Option<i32>) -> u32 {
    index
        .map(|value| (value.max(0) as u32).saturating_add(1))
        .unwrap_or(0)
}

/// 把 sidecar 快照映射为任务进度并等它结束。sidecar 自身仍负责执行；
/// 注册表只提供统一的状态/事件/取消通道（客户端无需再轮询 sidecar 专用快照）。
fn spawn_agent_worker(state: AppState, user_ns: String, task_id: String, job_id: String) {
    let service = state.task_service.clone();
    let worker_service = service.clone();
    let worker_task_id = task_id.clone();
    let handle = tokio::spawn(async move {
        let service = worker_service;
        let task_id = worker_task_id;
        service.mark_running(&task_id);
        let Some((cancel_flag, cancel_notify)) = service.cancellation(&task_id) else {
            return;
        };
        let mut guard = AgentTaskGuard {
            service: service.clone(),
            id: task_id.clone(),
            done: false,
        };
        let mut last: Option<(u32, u32, Option<String>, Option<String>)> = None;
        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                // 只有 sidecar 当前仍是本 worker 的任务时才杀进程；
                // 快照已被后续任务覆盖时不能误杀别的任务。
                let snapshot = state.agent_sidecar_service.status(&user_ns).await;
                if snapshot.job_id == job_id {
                    state.agent_sidecar_service.cancel_task(&user_ns).await;
                }
                let (done, total) = last
                    .as_ref()
                    .map(|(done, total, _, _)| (*done, *total))
                    .unwrap_or((0, 0));
                finish_agent_cancelled(&service, &task_id, done, total);
                guard.done = true;
                return;
            }
            let snapshot = state.agent_sidecar_service.status(&user_ns).await;
            if snapshot.job_id != job_id {
                // 注册表只剩「最新一条快照」：新任务启动后旧任务再也看不到
                // 自己的终态。这里主动收尾，避免永久占用任务名额。
                service.emit_progress_with_phase(
                    &task_id,
                    0,
                    0,
                    Some("任务状态已被后续任务取代".to_string()),
                    Some("error".to_string()),
                );
                service.finish(
                    &task_id,
                    TaskState::Failed,
                    Some(
                        Problem::new(ErrorCode::Internal)
                            .detail("任务状态已被后续任务取代，无法确认执行结果"),
                    ),
                );
                guard.done = true;
                return;
            }
            let done = chapter_count(snapshot.current_chapter_index);
            let total = chapter_count(snapshot.target_chapter_index);
            let message =
                (!snapshot.status_text.trim().is_empty()).then(|| snapshot.status_text.clone());
            let phase = Some(snapshot.phase.clone());
            let current = (done, total, message.clone(), phase.clone());
            if last.as_ref() != Some(&current) {
                service.emit_progress_with_phase(&task_id, done, total, message.clone(), phase);
                last = Some(current);
            }
            if !snapshot.running {
                let failed = snapshot.last_error.filter(|error| !error.trim().is_empty());
                // 终态阶段写回 progress：客户端无需把 state 与 phase 两个字段拼起来
                service.emit_progress_with_phase(
                    &task_id,
                    done,
                    total,
                    failed.clone().or(message),
                    Some(if failed.is_some() {
                        "error".to_string()
                    } else {
                        "idle".to_string()
                    }),
                );
                if let Some(error) = failed {
                    service.finish(
                        &task_id,
                        TaskState::Failed,
                        Some(Problem::new(ErrorCode::UpstreamFailed).detail(error)),
                    );
                } else {
                    service.finish(&task_id, TaskState::Succeeded, None);
                }
                guard.done = true;
                return;
            }
            tokio::select! {
                _ = cancel_notify.notified() => {}
                _ = tokio::time::sleep(AGENT_POLL_INTERVAL) => {}
            }
        }
    });
    // 注册 abort 句柄：取消 watchdog（5s）能真正终止卡死的工作器。
    // sidecar 进程由取消入口先行终止（通用 DELETE / 书级 DELETE），
    // abort 只负责回收 Rust 侧轮询任务。
    service.register_worker(&task_id, handle.abort_handle());
}

fn finish_agent_cancelled(service: &TaskService, task_id: &str, done: u32, total: u32) {
    service.emit_progress_with_phase(
        task_id,
        done,
        total,
        Some("已取消".to_string()),
        Some("idle".to_string()),
    );
    service.finish(task_id, TaskState::Cancelled, None);
}

// ─── AI 模型代理 ────────────────────────────────────────────────────────

fn proxy_problem(error: AppError) -> Problem {
    match error {
        AppError::BadRequest(detail) => Problem::new(ErrorCode::MalformedRequest).detail(detail),
        AppError::Blocked(detail) => Problem::new(ErrorCode::UpstreamBlocked).detail(detail),
        // 网络层失败（DNS/TLS/连接重置）：不把 reqwest 的 URL 细节带出去
        AppError::Http(_) => Problem::new(ErrorCode::UpstreamFailed).detail("模型服务请求失败"),
        other => Problem::from(other),
    }
}

/// 通用模型代理：沿用 `/reader3/aiProxy` 的转发实现，成功流式透传。
#[utoipa::path(
    post,
    path = "/v1/ai/proxy",
    tag = "ai",
    operation_id = "aiProxy",
    request_body = AiProxyRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "上游响应原样透传（JSON 或音频/图片流）", content_type = "application/octet-stream"),
        (status = 400, description = "URL/路径非法或被出站守卫拦截", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "请求体超过 8MB", body = Problem, content_type = "application/problem+json"),
        (status = 502, description = "模型服务失败（上游状态码在 upstreamCode）", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn proxy(
    State(state): State<AppState>,
    _user: V1User,
    V1Json(request): V1Json<AiProxyRequest>,
) -> Result<Response, Problem> {
    proxy_request(&state, request, ProxyErrorStyle::Problem)
        .await
        .map_err(proxy_problem)
}

/// 图片代理：拉取远程位图并回传（上限 20MB，SVG/HTML 一律拒绝）。
#[utoipa::path(
    post,
    path = "/v1/ai/proxy-image",
    tag = "ai",
    operation_id = "aiProxyImage",
    request_body = AiProxyImageRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "图片字节（image/jpeg 等位图）", content_type = "application/octet-stream"),
        (status = 400, description = "URL 非法、被出站守卫拦截或上游不是位图", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "请求体超过 8MB", body = Problem, content_type = "application/problem+json"),
        (status = 502, description = "模型服务失败（上游状态码在 upstreamCode）", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn proxy_image(
    V1User(_user): V1User,
    V1Json(request): V1Json<AiProxyImageRequest>,
) -> Result<Response, Problem> {
    proxy_image_request(request, ProxyErrorStyle::Problem)
        .await
        .map_err(proxy_problem)
}
