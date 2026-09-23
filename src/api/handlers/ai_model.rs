use axum::{extract::State, Json};
use serde_json::Value;

use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::ai_model::AiModelConfig;

/// 任何已登录用户都可读取；非管理员的响应会剔除各 apiKey。
pub async fn get_ai_model_config(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let is_admin = user.0.is_admin;
    let can_use_server_model = user.0.can_use_ai_model();
    let config = state.ai_model_service.get().await?;
    let visible_config = if is_admin {
        config
    } else {
        config.without_secrets()
    };
    Ok(Json(ApiResponse::ok(serde_json::json!({
        "config": visible_config,
        "canUseServerModel": can_use_server_model,
        "isAdmin": is_admin,
    }))))
}

/// 管理员专用：路由层已挂 `require_admin`。
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
