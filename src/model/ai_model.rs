use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct AiModelEndpointConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub use_full_url: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AiImageModelConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub use_full_url: bool,
    pub image_size: String,
}

impl Default for AiImageModelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            api_key: String::new(),
            model: "gpt-image-1".to_string(),
            use_full_url: false,
            image_size: "1024x1024".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AiSpeechModelConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub use_full_url: bool,
    pub voice: String,
    pub response_format: String,
}

impl Default for AiSpeechModelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            api_key: String::new(),
            model: "gpt-4o-mini-tts".to_string(),
            use_full_url: false,
            voice: "alloy".to_string(),
            response_format: "mp3".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct AiModelConfig {
    pub text: AiModelEndpointConfig,
    pub image: AiImageModelConfig,
    pub speech: AiSpeechModelConfig,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AiModelKind {
    Text,
    Image,
    Speech,
}

#[derive(Debug, Clone)]
pub struct ResolvedAiModelEndpoint {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub use_full_url: bool,
    pub image_size: Option<String>,
    pub voice: Option<String>,
    pub response_format: Option<String>,
}

impl AiModelConfig {
    /// 从环境变量读取后端 AI 模型配置。
    ///
    /// 后端配置只在服务端保存（env / .env），**从不下发到浏览器**——
    /// 前端只能拿到各模型的启用状态布尔值。
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).unwrap_or_default())
    }

    /// 与 `from_env` 同形，但来源可注入（测试用，避免碰进程环境变量）。
    pub fn from_lookup(lookup: impl Fn(&str) -> String) -> Self {
        let text = |key: &str| lookup(key).trim().to_string();
        let flag = |key: &str| {
            matches!(
                text(key).to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        };
        Self {
            text: AiModelEndpointConfig {
                enabled: flag("AI_TEXT_ENABLED"),
                base_url: text("AI_TEXT_BASE_URL"),
                api_key: text("AI_TEXT_API_KEY"),
                model: text("AI_TEXT_MODEL"),
                use_full_url: flag("AI_TEXT_USE_FULL_URL"),
            },
            image: AiImageModelConfig {
                enabled: flag("AI_IMAGE_ENABLED"),
                base_url: text("AI_IMAGE_BASE_URL"),
                api_key: text("AI_IMAGE_API_KEY"),
                model: text("AI_IMAGE_MODEL"),
                use_full_url: flag("AI_IMAGE_USE_FULL_URL"),
                image_size: text("AI_IMAGE_SIZE"),
            },
            speech: AiSpeechModelConfig {
                enabled: flag("AI_SPEECH_ENABLED"),
                base_url: text("AI_SPEECH_BASE_URL"),
                api_key: text("AI_SPEECH_API_KEY"),
                model: text("AI_SPEECH_MODEL"),
                use_full_url: flag("AI_SPEECH_USE_FULL_URL"),
                voice: text("AI_SPEECH_VOICE"),
                response_format: text("AI_SPEECH_FORMAT"),
            },
        }
        .sanitized()
    }
}

impl AiModelConfig {
    pub fn sanitized(mut self) -> Self {
        self.text.base_url = normalize_url(self.text.base_url);
        self.text.api_key = self.text.api_key.trim().to_string();
        self.text.model = self.text.model.trim().to_string();

        self.image.base_url = normalize_url(self.image.base_url);
        self.image.api_key = self.image.api_key.trim().to_string();
        self.image.model = self.image.model.trim().to_string();
        self.image.image_size = default_if_empty(self.image.image_size, "1024x1024");

        self.speech.base_url = normalize_url(self.speech.base_url);
        self.speech.api_key = self.speech.api_key.trim().to_string();
        self.speech.model = default_if_empty(self.speech.model, "gpt-4o-mini-tts");
        self.speech.voice = default_if_empty(self.speech.voice, "alloy");
        self.speech.response_format = default_if_empty(self.speech.response_format, "mp3");
        self
    }

    pub fn resolve(&self, kind: AiModelKind) -> ResolvedAiModelEndpoint {
        match kind {
            AiModelKind::Text => ResolvedAiModelEndpoint {
                enabled: self.text.enabled,
                base_url: self.text.base_url.clone(),
                api_key: self.text.api_key.clone(),
                model: self.text.model.clone(),
                use_full_url: self.text.use_full_url,
                image_size: None,
                voice: None,
                response_format: None,
            },
            AiModelKind::Image => ResolvedAiModelEndpoint {
                enabled: self.image.enabled,
                base_url: self.image.base_url.clone(),
                api_key: self.image.api_key.clone(),
                model: self.image.model.clone(),
                use_full_url: self.image.use_full_url,
                image_size: Some(self.image.image_size.clone()),
                voice: None,
                response_format: None,
            },
            AiModelKind::Speech => ResolvedAiModelEndpoint {
                enabled: self.speech.enabled,
                base_url: self.speech.base_url.clone(),
                api_key: self.speech.api_key.clone(),
                model: self.speech.model.clone(),
                use_full_url: self.speech.use_full_url,
                image_size: None,
                voice: Some(self.speech.voice.clone()),
                response_format: Some(self.speech.response_format.clone()),
            },
        }
    }
}

fn normalize_url(value: String) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn default_if_empty(value: String, default_value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        default_value.to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn from_lookup_reads_flat_env_vars() {
        let vars: HashMap<String, String> = [
            ("AI_TEXT_ENABLED", "true"),
            ("AI_TEXT_BASE_URL", "https://api.example.test/"),
            ("AI_TEXT_API_KEY", " sk-live "),
            ("AI_TEXT_MODEL", "gpt-4o-mini"),
            ("AI_SPEECH_ENABLED", "1"),
            ("AI_SPEECH_MODEL", ""),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let config = AiModelConfig::from_lookup(|key| vars.get(key).cloned().unwrap_or_default());
        assert!(config.text.enabled);
        assert_eq!(config.text.base_url, "https://api.example.test");
        assert_eq!(config.text.api_key, "sk-live");
        assert_eq!(config.text.model, "gpt-4o-mini");
        assert!(config.speech.enabled);
        // 空值落回默认
        assert_eq!(config.speech.model, "gpt-4o-mini-tts");
        assert!(!config.image.enabled);
    }

    #[test]
    fn from_lookup_defaults_to_disabled() {
        let config = AiModelConfig::from_lookup(|_| String::new());
        assert!(!config.text.enabled && !config.image.enabled && !config.speech.enabled);
    }
}
