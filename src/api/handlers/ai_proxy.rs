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
use crate::model::ai_model::{AiModelKind, ResolvedAiModelEndpoint};
use crate::model::ai_proxy::{
    ai_proxy_timeout, build_ai_proxy_url, format_ai_proxy_upstream_error,
    validate_ai_proxy_image_url, AiProxyImageRequest, AiProxyRequest,
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
    use super::is_allowed_proxy_image_content_type;

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
}
