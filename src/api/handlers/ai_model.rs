use axum::{extract::State, Json};
use serde_json::Value;

use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::ai_model::AiModelConfig;

/// 已登录即可读取完整配置（含 apiKey）：单用户部署没有可见性分层。
pub async fn get_ai_model_config(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let config = state.ai_model_service.get().await?;
    Ok(Json(ApiResponse::ok(serde_json::json!({
        "config": config,
        "canUseServerModel": true,
        "isAdmin": true,
    }))))
}

/// 保存服务端 AI 模型配置（路由层需登录）。
pub async fn save_ai_model_config(
    State(state): State<AppState>,
    Json(config): Json<AiModelConfig>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let saved = state.ai_model_service.save(config).await?;
    Ok(Json(ApiResponse::ok(serde_json::json!({
        "config": saved,
        "canUseServerModel": true,
        "isAdmin": true,
    }))))
}
