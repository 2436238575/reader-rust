use crate::model::ai_model::AiModelKind;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use url::Url;

const AI_PROXY_TIMEOUT_SECS: u64 = 300;
const ALLOWED_PROXY_PATHS: [&str; 3] = [
    "/v1/chat/completions",
    "/v1/images/generations",
    "/v1/audio/speech",
];

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiProxyRequest {
    #[serde(default)]
    pub base_url: String,
    pub api_key: Option<String>,
    pub path: String,
    #[serde(default)]
    pub full_url: bool,
    #[serde(default)]
    pub use_server_config: bool,
    pub kind: Option<AiModelKind>,
    /// 透传给上游的请求体（结构随模型接口而定）
    #[schema(value_type = Object)]
    pub body: Value,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct AiProxyImageRequest {
    pub url: String,
}

pub fn build_ai_proxy_url(base_url: &str, path: &str, full_url: bool) -> Result<Url, String> {
    if full_url {
        return parse_http_url(base_url);
    }

    if !ALLOWED_PROXY_PATHS.contains(&path) {
        return Err(format!("unsupported proxy path: {}", path));
    }

    let mut base = parse_http_url(base_url)?;
    // OpenAI 兼容服务的 BASE_URL 约定混乱：有人填到主机（…:8825），有人填到
    // `/v1`（…:8825/v1）。后者直接拼会出 `/v1/v1/chat/completions` 404，
    // 这里把重复的 `/v1` 段折叠掉（BASE_URL 里再深的前缀如 /api/v1 也一样折叠）
    let base_path = base.path().trim_end_matches('/');
    let suffix = if base_path.ends_with("/v1") {
        path.strip_prefix("/v1").unwrap_or(path)
    } else {
        path
    };
    let joined_path = format!("{}{}", base_path, suffix);
    base.set_path(&joined_path);
    base.set_query(None);
    base.set_fragment(None);
    Ok(base)
}

pub fn validate_ai_proxy_image_url(url: &str) -> Result<Url, String> {
    parse_http_url(url)
}

pub fn ai_proxy_timeout() -> Duration {
    Duration::from_secs(AI_PROXY_TIMEOUT_SECS)
}

pub fn format_ai_proxy_upstream_error(status: u16, body: &str) -> String {
    let reason = http::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Upstream Error");
    let detail = extract_error_detail(body);
    if detail.is_empty() {
        return format!("模型服务返回 {} {}", status, reason);
    }
    format!("模型服务返回 {} {}：{}", status, reason, detail)
}

/// mimo chat 协议下，上游 200 却解析不出音频数据时的用户可读文案。
///
/// 与 `format_ai_proxy_upstream_error` 同风格：统一前缀 + 上游响应片段
/// （截断到 240 字符），用于排查是 JSON 损坏还是 audio.data 缺失/损坏。
pub fn format_ai_proxy_speech_decode_error(body: &str) -> String {
    let detail = extract_error_detail(body);
    if detail.is_empty() {
        return "模型服务返回的语音响应无法解析".to_string();
    }
    format!("模型服务返回的语音响应无法解析：{detail}")
}

fn parse_http_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|e| e.to_string())?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        _ => Err("only http/https proxy targets are supported".to_string()),
    }
}

fn extract_error_detail(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(message) = value
            .pointer("/error/message")
            .or_else(|| value.get("errorMsg"))
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
        {
            return truncate_error_detail(message);
        }
    }

    truncate_error_detail(&crate::util::text::strip_html_tags(body))
}

fn truncate_error_detail(value: &str) -> String {
    let cleaned = value.trim();
    if cleaned.chars().count() <= 240 {
        return cleaned.to_string();
    }
    let mut result = cleaned.chars().take(240).collect::<String>();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_path_onto_bare_host() {
        let url =
            build_ai_proxy_url("http://localhost:8825", "/v1/chat/completions", false).unwrap();
        assert_eq!(url.as_str(), "http://localhost:8825/v1/chat/completions");
    }

    #[test]
    fn folds_duplicate_v1_segment() {
        // BASE_URL 带 /v1 尾巴时不再叠出 /v1/v1/...（上游 404 的常见根因）
        let url =
            build_ai_proxy_url("http://localhost:8825/v1", "/v1/chat/completions", false).unwrap();
        assert_eq!(url.as_str(), "http://localhost:8825/v1/chat/completions");
        let url =
            build_ai_proxy_url("http://localhost:8825/v1/", "/v1/audio/speech", false).unwrap();
        assert_eq!(url.as_str(), "http://localhost:8825/v1/audio/speech");
        // 更深的前缀同理
        let url = build_ai_proxy_url(
            "https://proxy.example/api/v1",
            "/v1/images/generations",
            false,
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://proxy.example/api/v1/images/generations"
        );
    }

    #[test]
    fn keeps_non_v1_prefix_and_rejects_unknown_path() {
        let url = build_ai_proxy_url(
            "https://proxy.example/openai",
            "/v1/chat/completions",
            false,
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://proxy.example/openai/v1/chat/completions"
        );
        assert!(build_ai_proxy_url("http://localhost:8825", "/v1/models", false).is_err());
    }

    #[test]
    fn speech_decode_error_carries_upstream_detail() {
        // mimo 错误/无音频字段的 JSON：取 error.message 或响应片段
        let message = format_ai_proxy_speech_decode_error(
            r#"{"error":{"code":"400","message":"Param Incorrect"}}"#,
        );
        assert_eq!(message, "模型服务返回的语音响应无法解析：Param Incorrect");
        // 空响应只给统一前缀
        assert_eq!(
            format_ai_proxy_speech_decode_error(""),
            "模型服务返回的语音响应无法解析"
        );
    }
}
