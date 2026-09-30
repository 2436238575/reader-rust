use axum::{extract::State, Json};
use serde_json::Value;

use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::ai_model::AiModelConfig;

fn endpoint_ready(enabled: bool, base_url: &str, model: &str) -> bool {
    enabled && !base_url.trim().is_empty() && !model.trim().is_empty()
}

/// 后端 AI 模型只回报「是否配置可用」的布尔状态。
///
/// 配置本体（地址/Key/模型名）只存在于服务端 env，从不下发到浏览器——
/// 浏览器经 `aiProxy` 间接使用，全程接触不到 Key。
pub async fn get_ai_model_config(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let AiModelConfig {
        text,
        image,
        speech,
    } = state.ai_model_service.get().await?;
    let text_ready = endpoint_ready(text.enabled, &text.base_url, &text.model);
    let image_ready = endpoint_ready(image.enabled, &image.base_url, &image.model);
    let speech_ready = endpoint_ready(speech.enabled, &speech.base_url, &speech.model);
    Ok(Json(ApiResponse::ok(serde_json::json!({
        "canUseServerModel": text_ready || image_ready || speech_ready,
        "textReady": text_ready,
        "imageReady": image_ready,
        "speechReady": speech_ready,
        // sidecar 可用性：仅提示用，失败不影响其他 AI 功能
        "agentReady": state.agent_sidecar_service.agent_ready(),
    }))))
}
