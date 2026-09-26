use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::Value;

use crate::api::AppState;
use crate::error::error::{ApiResponse, AppError};

#[derive(Debug, Deserialize)]
pub struct VersionUpdateQuery {
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct DismissVersionUpdateRequest {
    pub version: Option<String>,
}

/// 版本更新检查与忽略（路由层需登录）。
pub async fn get_version_update(
    State(state): State<AppState>,
    Query(query): Query<VersionUpdateQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let info = state
        .update_service
        .check(query.force.unwrap_or(false))
        .await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(info).map_err(|err| AppError::BadRequest(err.to_string()))?,
    )))
}

/// 版本更新检查与忽略（路由层需登录）。
pub async fn dismiss_version_update(
    State(state): State<AppState>,
    Json(req): Json<DismissVersionUpdateRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let version = req.version.unwrap_or_default();
    let info = state.update_service.dismiss(&version).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(info).map_err(|err| AppError::BadRequest(err.to_string()))?,
    )))
}
