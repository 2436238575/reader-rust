use axum::{
    extract::State,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;

use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::ai_model::{AiModelKind, ResolvedAiModelEndpoint, AI_SPEECH_PROTOCOL_MIMO_CHAT};
use crate::model::ai_proxy::{
    ai_proxy_timeout, build_ai_proxy_url, format_ai_proxy_speech_decode_error,
    format_ai_proxy_upstream_error, validate_ai_proxy_image_url, AiProxyImageRequest,
    AiProxyRequest,
};

const MAX_PROXY_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// 通用 AI 代理响应体上限。
const MAX_PROXY_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

pub async fn ai_proxy(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<AiProxyRequest>,
) -> Result<Response, AppError> {
    let (endpoint, kind, path, mut body) = resolve_ai_proxy_target(&state, &user, req).await?;
    if let Some(kind) = kind {
        apply_server_model_body_defaults(&endpoint, kind, &mut body);
    }
    // 小米 mimo 的 TTS 不提供 OpenAI /v1/audio/speech，借用 chat 协议单独分派
    // （只作用于服务端配置的 speech 端点；客户端自带端点按其所选路径原样转发）
    if kind == Some(AiModelKind::Speech)
        && endpoint.speech_protocol.as_deref() == Some(AI_SPEECH_PROTOCOL_MIMO_CHAT)
    {
        return ai_proxy_mimo_speech(endpoint, body).await;
    }
    let target = build_ai_proxy_url(&endpoint.base_url, &path, endpoint.use_full_url)
        .map_err(AppError::BadRequest)?;
    // 出站守卫：use_server_config=false 时 base_url 完全由客户端提供
    crate::crawler::url_guard::ensure_outbound_url_allowed(&target)
        .await
        .map_err(AppError::BadRequest)?;
    let client = ai_proxy_client();
    let mut builder = client
        .post(target)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&body);

    if let Some(api_key) = Some(str::trim(endpoint.api_key.as_str())).filter(|v| !v.is_empty()) {
        builder = builder.bearer_auth(api_key);
    }

    let upstream = builder.send().await.map_err(map_ai_proxy_http_error)?;
    response_from_upstream(upstream).await
}

async fn resolve_ai_proxy_target(
    state: &AppState,
    _user: &CurrentUser,
    req: AiProxyRequest,
) -> Result<(ResolvedAiModelEndpoint, Option<AiModelKind>, String, Value), AppError> {
    if req.use_server_config {
        let kind = req.kind.unwrap_or_else(|| infer_ai_model_kind(&req.path));
        let config = state.ai_model_service.get().await?;
        let endpoint = config.resolve(kind);
        if !endpoint.enabled
            || endpoint.base_url.trim().is_empty()
            || endpoint.model.trim().is_empty()
        {
            return Err(AppError::BadRequest(
                "后端模型配置未启用或不完整".to_string(),
            ));
        }
        return Ok((
            endpoint,
            Some(kind),
            default_ai_model_path(kind).to_string(),
            req.body,
        ));
    }

    // 客户端自带端点（含 fullUrl 任意路径）等于把服务端当成向任意地址发 POST
    // 的通用代理。单用户部署下登录者即所有者，不额外收敛；出站安全由
    // `ALLOW_PRIVATE_NETWORK` 的 URL 守卫兜底。
    Ok((
        ResolvedAiModelEndpoint {
            enabled: true,
            base_url: req.base_url,
            api_key: req.api_key.unwrap_or_default(),
            model: String::new(),
            use_full_url: req.full_url,
            image_size: None,
            voice: None,
            response_format: None,
            // 协议是服务端配置的属性：客户端自带端点永远按 OpenAI 路径语义转发
            speech_protocol: None,
        },
        None,
        req.path,
        req.body,
    ))
}

fn infer_ai_model_kind(path: &str) -> AiModelKind {
    match path {
        "/v1/images/generations" => AiModelKind::Image,
        "/v1/audio/speech" => AiModelKind::Speech,
        _ => AiModelKind::Text,
    }
}

fn default_ai_model_path(kind: AiModelKind) -> &'static str {
    match kind {
        AiModelKind::Text => "/v1/chat/completions",
        AiModelKind::Image => "/v1/images/generations",
        AiModelKind::Speech => "/v1/audio/speech",
    }
}

fn apply_server_model_body_defaults(
    endpoint: &ResolvedAiModelEndpoint,
    kind: AiModelKind,
    body: &mut Value,
) {
    if endpoint.model.is_empty() {
        return;
    }
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    obj.insert("model".to_string(), Value::String(endpoint.model.clone()));
    if kind == AiModelKind::Image {
        if let Some(size) = endpoint
            .image_size
            .as_ref()
            .filter(|v| !v.trim().is_empty())
        {
            obj.insert("size".to_string(), Value::String(size.clone()));
        }
    }
    if kind == AiModelKind::Speech {
        if let Some(voice) = endpoint.voice.as_ref().filter(|v| !v.trim().is_empty()) {
            obj.insert("voice".to_string(), Value::String(voice.clone()));
        }
        if let Some(format) = endpoint
            .response_format
            .as_ref()
            .filter(|v| !v.trim().is_empty())
        {
            obj.insert("response_format".to_string(), Value::String(format.clone()));
        }
    }
}

/// mimo chat 协议的 speech 代理。
///
/// 小米 mimo（`mimo-v2.5-tts`）不提供 OpenAI 的 `/v1/audio/speech`，而是
/// 借用 `/v1/chat/completions`：assistant 角色的 content 就是要朗读的文本，
/// 音频以 base64 WAV 藏在 `choices[0].message.audio.data`。voice/speed/
/// response_format 上游未证实支持，重建请求体时不带（实测不带即 200，
/// 用默认音色）。
async fn ai_proxy_mimo_speech(
    endpoint: ResolvedAiModelEndpoint,
    body: Value,
) -> Result<Response, AppError> {
    let payload =
        build_mimo_speech_request(&endpoint.model, &body).map_err(AppError::BadRequest)?;
    let target = build_ai_proxy_url(
        &endpoint.base_url,
        "/v1/chat/completions",
        endpoint.use_full_url,
    )
    .map_err(AppError::BadRequest)?;
    // 出站守卫：与通用分支同口径
    crate::crawler::url_guard::ensure_outbound_url_allowed(&target)
        .await
        .map_err(AppError::BadRequest)?;
    let client = ai_proxy_client();
    let mut builder = client
        .post(target)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&payload);
    if let Some(api_key) = Some(str::trim(endpoint.api_key.as_str())).filter(|v| !v.is_empty()) {
        builder = builder.bearer_auth(api_key);
    }
    let upstream = builder.send().await.map_err(map_ai_proxy_http_error)?;
    mimo_speech_response(upstream).await
}

/// 构造 mimo chat 协议请求体。
///
/// 客户端发来的是 OpenAI speech 形态，服务端整体重构造为 chat 形态，
/// 只取 `input` 作为待合成文本；缺失、为空或非字符串视为参数错误
/// （上层转 400）。
fn build_mimo_speech_request(model: &str, client_body: &Value) -> Result<Value, String> {
    let input = client_body
        .get("input")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if input.is_empty() {
        return Err("缺少待合成文本（body.input）".to_string());
    }
    Ok(serde_json::json!({
        "model": model,
        "messages": [{ "role": "assistant", "content": input }],
    }))
}

/// mimo chat 协议的响应处理：整包读 JSON 后解出 WAV 字节。
async fn mimo_speech_response(upstream: reqwest::Response) -> Result<Response, AppError> {
    let status = upstream.status();
    // 音频 base64 藏在 JSON 里，必须整包读完才能解码；限量与通用分支一致
    let body = crate::crawler::fetcher::read_body_limited(upstream, MAX_PROXY_RESPONSE_BYTES)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    if !status.is_success() {
        return Ok(build_upstream_error_response(status, &body));
    }
    match decode_mimo_speech_audio(&body) {
        Some(wav) => Ok(build_response(
            status,
            Some(HeaderValue::from_static("audio/wav")),
            wav,
        )),
        // 上游 200 但拿不到音频：按网关错误回 502，文案带上游响应片段
        None => Ok(build_mimo_speech_decode_error(&body)),
    }
}

/// 从 mimo chat 协议响应里解出音频字节。
///
/// 音频在 `choices[0].message.audio.data`，是 base64 编码的 WAV；
/// JSON 损坏、字段缺失或 base64 损坏都返回 None（上层转 502）。
fn decode_mimo_speech_audio(body: &[u8]) -> Option<bytes::Bytes> {
    use base64::Engine;
    let value = serde_json::from_slice::<Value>(body).ok()?;
    let data = value
        .pointer("/choices/0/message/audio/data")
        .and_then(Value::as_str)?;
    let wav = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Some(bytes::Bytes::from(wav))
}

/// 上游 200 但解析不出音频时的 502 响应（文案风格与通用上游错误一致）。
fn build_mimo_speech_decode_error(body: &bytes::Bytes) -> Response {
    let body_text = std::str::from_utf8(body).unwrap_or("");
    let message = format_ai_proxy_speech_decode_error(body_text);
    let mut response = Json(ApiResponse::<Value>::err(message)).into_response();
    *response.status_mut() = StatusCode::BAD_GATEWAY;
    response
}

pub async fn ai_proxy_image(Json(req): Json<AiProxyImageRequest>) -> Result<Response, AppError> {
    let target = validate_ai_proxy_image_url(&req.url).map_err(AppError::BadRequest)?;
    // 出站守卫：url 由客户端提供
    crate::crawler::url_guard::ensure_outbound_url_allowed(&target)
        .await
        .map_err(AppError::BadRequest)?;
    let client = ai_proxy_client();
    let upstream = client
        .get(target)
        .header(reqwest::header::ACCEPT, "image/*,*/*;q=0.8")
        .send()
        .await
        .map_err(map_ai_proxy_http_error)?;

    if let Some(length) = upstream.content_length() {
        if length > MAX_PROXY_IMAGE_BYTES {
            return Err(AppError::BadRequest("图片超过代理大小限制".to_string()));
        }
    }

    let status = upstream.status();
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| HeaderValue::from_str(v).ok());
    let body = crate::crawler::fetcher::read_body_limited(upstream, MAX_PROXY_IMAGE_BYTES)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    // read_body_limited 内部已做限量，超限在这里不可达
    if !status.is_success() {
        return Ok(build_upstream_error_response(status, &body));
    }
    // 上游 Content-Type 原样透传是自 XSS 面（客户端可控 url 让上游回
    // text/html）：图片代理只放行位图格式；image/svg+xml 等可携带脚本
    // 的类型与其他非图片类型一律拒绝
    let allowed = content_type
        .as_ref()
        .and_then(|v| v.to_str().ok())
        .is_some_and(is_allowed_proxy_image_content_type);
    if !allowed {
        return Err(AppError::BadRequest(
            "上游返回的不是受支持的图片格式（仅 jpg/png/gif/webp/avif/bmp）".to_string(),
        ));
    }
    Ok(build_response(status, content_type, body))
}

/// 图片代理放行的上游 Content-Type：只认位图。SVG 在同源直出时可携带
/// 脚本（自 XSS），与 HTML 一样不可透传。
fn is_allowed_proxy_image_content_type(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(
        mime.as_str(),
        "image/jpeg" | "image/png" | "image/gif" | "image/webp" | "image/avif" | "image/bmp"
    )
}

async fn response_from_upstream(upstream: reqwest::Response) -> Result<Response, AppError> {
    let status = upstream.status();
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| HeaderValue::from_str(v).ok());
    if !status.is_success() {
        // 错误响应体小、且要拼进用户可读文案：整读限量
        let body = crate::crawler::fetcher::read_body_limited(upstream, MAX_PROXY_RESPONSE_BYTES)
            .await
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        return Ok(build_upstream_error_response(status, &body));
    }
    // 成功响应流式转发：整包缓冲会把 stream:true 的客户端（TTS 大音频、长文本）
    // 憋到上游结束才一次性吐出，还会撞整包上限。流上累计计数做截断兜底：
    // 超限即掐断（客户端收到截断响应，比整包进内存强）
    let mut total = 0u64;
    let capped = futures::stream::StreamExt::map(upstream.bytes_stream(), move |item| {
        item.map_err(std::io::Error::other).and_then(|chunk| {
            total += chunk.len() as u64;
            if total > MAX_PROXY_RESPONSE_BYTES {
                return Err(std::io::Error::other("上游响应超过代理大小限制"));
            }
            Ok(chunk)
        })
    });
    Ok(build_stream_response(status, content_type, capped))
}

fn build_stream_response<S>(
    status: reqwest::StatusCode,
    content_type: Option<HeaderValue>,
    stream: S,
) -> Response
where
    S: futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
{
    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = (status, axum::body::Body::from_stream(stream)).into_response();
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    response
}

fn build_response(
    status: reqwest::StatusCode,
    content_type: Option<HeaderValue>,
    body: bytes::Bytes,
) -> Response {
    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = (status, body).into_response();
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    response
}

fn build_upstream_error_response(status: reqwest::StatusCode, body: &bytes::Bytes) -> Response {
    let body_text = std::str::from_utf8(body).unwrap_or("");
    let message = format_ai_proxy_upstream_error(status.as_u16(), body_text);
    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = Json(ApiResponse::<Value>::err(message)).into_response();
    *response.status_mut() = status;
    response
}

/// AI 代理客户端：进程级复用（连接池/TLS 握手跨请求共享）。
/// 配置全是静态的（超时常量 + 固定重定向守卫），无需按请求构建。
static AI_PROXY_CLIENT: once_cell::sync::Lazy<reqwest::Client> = once_cell::sync::Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(ai_proxy_timeout())
        .redirect(crate::crawler::url_guard::guarded_redirect_policy())
        .dns_resolver(crate::crawler::url_guard::guarded_dns_resolver())
        // TLS 与全局一致：ring + webpki-roots（见 crawler::tls）
        .tls_backend_preconfigured(crate::crawler::tls::shared_client_config())
        .build()
        .expect("AI 代理 reqwest 客户端构建失败")
});

fn ai_proxy_client() -> reqwest::Client {
    AI_PROXY_CLIENT.clone()
}

fn map_ai_proxy_http_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        return AppError::BadRequest("模型服务请求超时，请检查模型地址或稍后重试".to_string());
    }
    AppError::Http(error)
}

#[cfg(test)]
mod tests {
    use super::{
        build_mimo_speech_request, decode_mimo_speech_audio, is_allowed_proxy_image_content_type,
    };

    #[test]
    fn proxy_image_content_type_allows_bitmaps_only() {
        for ct in [
            "image/jpeg",
            "image/png",
            "image/gif",
            "image/webp",
            "image/avif",
            "image/bmp",
            "IMAGE/PNG",
            "image/png; charset=binary",
        ] {
            assert!(is_allowed_proxy_image_content_type(ct), "{ct} 应放行");
        }
        // SVG 可携带脚本，HTML/文本是自 XSS 面，通配值不可信
        for ct in [
            "image/svg+xml",
            "text/html",
            "text/plain",
            "application/octet-stream",
            "image/",
            "",
        ] {
            assert!(!is_allowed_proxy_image_content_type(ct), "{ct} 应拒绝");
        }
    }

    #[test]
    fn mimo_speech_request_rebuilds_chat_body_from_input() {
        // 客户端发 OpenAI speech 形态，服务端整体重构造：只留 model +
        // assistant content，voice/response_format 等字段一律丢弃
        let client_body = serde_json::json!({
            "model": "gpt-4o-mini-tts",
            "input": "第一段 正文",
            "voice": "alloy",
            "response_format": "mp3",
        });
        let payload = build_mimo_speech_request("mimo-v2.5-tts", &client_body).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({
                "model": "mimo-v2.5-tts",
                "messages": [{ "role": "assistant", "content": "第一段 正文" }],
            })
        );
    }

    #[test]
    fn mimo_speech_request_rejects_missing_or_empty_input() {
        for body in [
            serde_json::json!({}),
            serde_json::json!({ "input": "" }),
            serde_json::json!({ "input": "   " }),
            // OpenAI 的 input 允许数组，mimo 只收纯文本：非字符串按缺失处理
            serde_json::json!({ "input": ["文本"] }),
        ] {
            assert!(
                build_mimo_speech_request("m", &body).is_err(),
                "{body} 应拒绝"
            );
        }
    }

    #[test]
    fn mimo_speech_audio_decodes_base64_wav() {
        // 最小 44 字节 WAV 头（RIFF/WAVE/fmt/data），base64 以 UklGRi 开头
        let payload = serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "audio": {
                        "data": "UklGRiQAAABXQVZFZm10IBAAAAABAAEAIlYAACJWAAABAAgAZGF0YQAAAAA="
                    },
                },
            }],
        })
        .to_string();
        let wav = decode_mimo_speech_audio(payload.as_bytes()).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(wav.len(), 44);
    }

    #[test]
    fn mimo_speech_audio_returns_none_on_bad_payloads() {
        // audio.data 缺失（纯文本 chat 响应）
        let missing = serde_json::json!({
            "choices": [{ "message": { "content": "普通文本回复" } }],
        })
        .to_string();
        assert!(decode_mimo_speech_audio(missing.as_bytes()).is_none());
        // base64 损坏
        let broken_b64 = serde_json::json!({
            "choices": [{ "message": { "audio": { "data": "不是base64!!" } } }],
        })
        .to_string();
        assert!(decode_mimo_speech_audio(broken_b64.as_bytes()).is_none());
        // JSON 损坏 / 空响应
        assert!(decode_mimo_speech_audio(b"not json").is_none());
        assert!(decode_mimo_speech_audio(b"").is_none());
    }
}
