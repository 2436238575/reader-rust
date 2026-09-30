//! AI 资料编排任务的三个轮询端点。
//!
//! POST /reader3/runAgentTask    提交任务（update_to_current | redraw_map），进行中返回 409
//! POST /reader3/getAgentTaskStatus   状态快照（含 bookUrl，前端据此过滤他书任务）
//! POST /reader3/cancelAgentTask      取消 = kill sidecar 进程
//!
//! 结果不设独立端点：任务完成后前端复用 getAiBookMemory 重拉资料。

use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::Value;

use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::service::agent_sidecar_service::AgentTaskKind;
use crate::util::text::repair_encoded_url;

#[derive(Debug, Deserialize, Default)]
pub struct RunAgentTaskRequest {
    #[serde(rename = "bookUrl", alias = "url")]
    pub book_url: Option<String>,
    pub kind: Option<String>,
    #[serde(rename = "targetChapterIndex")]
    pub target_chapter_index: Option<i32>,
}

pub async fn run_agent_task(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<RunAgentTaskRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let book_url = req
        .book_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| AppError::BadRequest("bookUrl required".to_string()))?;
    let book_url = repair_encoded_url(book_url);
    let kind = AgentTaskKind::parse(req.kind.as_deref().unwrap_or("update_to_current"))
        .ok_or_else(|| {
            AppError::BadRequest("kind 必须是 update_to_current 或 redraw_map".to_string())
        })?;

    // 任务只针对书架上的书
    let book = state
        .book_service
        .get_shelf_book(&user_ns, &book_url)
        .await?
        .ok_or_else(|| AppError::BadRequest("书籍未加入书架".to_string()))?;

    let job_id = state
        .agent_sidecar_service
        .start_task(&user_ns, book, kind, req.target_chapter_index)
        .await?;
    Ok(Json(ApiResponse::ok(
        serde_json::json!({ "jobId": job_id }),
    )))
}

pub async fn get_agent_task_status(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let snapshot = state.agent_sidecar_service.status().await;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(snapshot).unwrap_or_default(),
    )))
}

pub async fn cancel_agent_task(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let cancelled = state.agent_sidecar_service.cancel_task().await;
    Ok(Json(ApiResponse::ok(
        serde_json::json!({ "cancelled": cancelled }),
    )))
}
