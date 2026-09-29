use crate::model::ai_model::AiModelConfig;

/// 后端 AI 模型配置。
///
/// 配置只来自启动时的环境变量（`AI_TEXT_*` / `AI_IMAGE_*` / `AI_SPEECH_*`），
/// 不落库、不提供保存接口：API Key 这类敏感配置从不下发到浏览器，
/// 前端只能拿到「某类模型是否已配置可用」的布尔状态。
#[derive(Clone)]
pub struct AiModelService {
    config: AiModelConfig,
}

impl AiModelService {
    pub fn new(config: AiModelConfig) -> Self {
        Self { config }
    }

    pub async fn get(&self) -> Result<AiModelConfig, crate::error::error::AppError> {
        Ok(self.config.clone())
    }
}
