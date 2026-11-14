//! `/v1/sources` 文件域：解析上传的书源文件、从远端 URL 拉取、全量导出。
//!
//! - `POST /v1/sources:parse`：multipart `.json/.txt`（≤32MB），只解析不落库；
//!   前端预览后自行调 `:batchUpsert`。
//! - `POST /v1/sources:fetch`：按 URL 拉取远端 JSON 再解析。**严格公网**：
//!   忽略部署级 `ALLOW_PRIVATE_NETWORK`/白名单，只允许 http/https，DNS 解析
//!   与每一次重定向都拒绝环回/私网/链路本地/保留地址；15 秒超时、响应 ≤32MB。
//! - `GET /v1/sources:export`：全量材料（含规则），`Content-Disposition:
//!   attachment` 供导出/备份。

use crate::api::handlers::book_source::{extract_sources, MAX_SOURCE_FILE_BYTES};
use crate::api::handlers::multipart::next_field_limited;
use crate::api::problem::{ErrorCode, FieldIssue, Problem};
use crate::api::v1::{V1Json, V1Multipart, V1User};
use crate::api::AppState;
use crate::crawler::fetcher::MAX_RESPONSE_BYTES;
use crate::model::book_source::BookSource;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::Deserialize;
use std::time::Duration;
use utoipa::ToSchema;

const FETCH_TIMEOUT_SECS: u64 = 15;

/// 解析上传的书源文件（multipart `file` 字段，`.json` / `.txt`）。
///
/// 只解析不落库：前端先预览，确认后再 `POST /v1/sources:batchUpsert`。
pub fn upload_router(state: AppState, upload_timeout: Duration) -> Router<AppState> {
    Router::new()
        .route("/v1/sources:parse", post(parse_source_file))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::api::v1::require_auth,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_SOURCE_FILE_BYTES + 1024 * 1024,
        ))
        .layer(axum::middleware::from_fn(crate::api::v1::require_csrf))
        .layer(axum::middleware::from_fn_with_state(
            (
                upload_timeout,
                crate::api::inbound_guard::TimeoutShape::Problem,
            ),
            crate::api::inbound_guard::timeout_middleware,
        ))
}

/// 远端拉取与导出走常规 v1 超时/体限（JSON 请求很小，上游请求自带 15s 预算）。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sources:fetch", post(fetch_source_file))
        .route("/v1/sources:export", get(export_sources))
}

fn parse_sources_text(text: &str) -> Result<Vec<BookSource>, Problem> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|error| {
        Problem::new(ErrorCode::ValidationFailed).detail(format!("书源文件不是合法 JSON：{error}"))
    })?;
    extract_sources(value)
        .map_err(|error| Problem::new(ErrorCode::ValidationFailed).detail(error.client_message()))
}

/// 解析上传的书源文件：只解析、不落库，返回 `Vec<BookSource>` 供预览。
#[utoipa::path(
    post,
    path = "/v1/sources:parse",
    tag = "sources",
    operation_id = "parseSourceFile",
    request_body(content = String, content_type = "multipart/form-data", description = "file 字段：.json / .txt（≤32MB）"),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "解析出的书源（未落库）", body = [BookSource]),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "文件超过 32MB", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "缺少文件 / 扩展名不支持 / JSON 或源材料非法", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn parse_source_file(
    _user: V1User,
    V1Multipart(mut multipart): V1Multipart,
) -> Result<Json<Vec<BookSource>>, Problem> {
    let mut field_count = 0usize;
    while let Some(field) = next_field_limited(&mut multipart, &mut field_count)
        .await
        .map_err(Problem::from)?
    {
        let Some(file_name) = field.file_name().map(str::to_string) else {
            continue;
        };
        let lower = file_name.to_ascii_lowercase();
        if !lower.ends_with(".json") && !lower.ends_with(".txt") {
            return Err(
                Problem::new(ErrorCode::ValidationFailed).detail("只支持 .json 或 .txt 书源文件")
            );
        }
        let mut field = field;
        let mut bytes = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(|error| {
            Problem::new(ErrorCode::MalformedRequest).detail(format!("读取书源文件失败：{error}"))
        })? {
            if bytes.len().saturating_add(chunk.len()) > MAX_SOURCE_FILE_BYTES {
                return Err(
                    Problem::new(ErrorCode::PayloadTooLarge).detail("书源文件不能超过 32MB")
                );
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&bytes);
        return Ok(Json(parse_sources_text(&text)?));
    }
    Err(Problem::new(ErrorCode::ValidationFailed).detail("缺少书源文件"))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FetchSourcesRequest {
    pub url: String,
}

/// 从远端 URL 拉取书源 JSON 并解析（严格公网 SSRF 策略）。
#[utoipa::path(
    post,
    path = "/v1/sources:fetch",
    tag = "sources",
    operation_id = "fetchSourceFile",
    request_body = FetchSourcesRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "解析出的书源（未落库）", body = [BookSource]),
        (status = 400, description = "URL 非法或指向私网/环回/保留地址", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "远端响应超过 32MB", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "JSON 或源材料非法", body = Problem, content_type = "application/problem+json"),
        (status = 502, description = "远端请求失败或返回非 2xx", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn fetch_source_file(
    _user: V1User,
    V1Json(request): V1Json<FetchSourcesRequest>,
) -> Result<Json<Vec<BookSource>>, Problem> {
    let raw = request.url.trim();
    if raw.is_empty() {
        return Err(Problem::new(ErrorCode::ValidationFailed)
            .detail("url 不能为空")
            .errors(vec![FieldIssue {
                pointer: "/url".to_string(),
                code: "REQUIRED".to_string(),
                detail: None,
            }]));
    }
    // 严格公网：不读部署级私网白名单，客户端给什么 URL 都只能访问公网。
    // 预检 DNS 也纳入 15s 预算（给 5s，剩余时间留给实际请求）。
    let target = tokio::time::timeout(
        Duration::from_secs(5),
        crate::crawler::url_guard::ensure_public_outbound_url_str_allowed(raw),
    )
    .await
    .map_err(|_| Problem::new(ErrorCode::UpstreamTimeout).detail("解析远端书源地址超时"))?
    .map_err(|error| Problem::new(ErrorCode::UpstreamBlocked).detail(error))?;
    let client = crate::crawler::url_guard::public_only_client_builder(
        Duration::from_secs(FETCH_TIMEOUT_SECS),
        crate::crawler::http_client::DEFAULT_USER_AGENT,
    )
    .build()
    .map_err(|error| {
        tracing::warn!(error = %error, "构建书源拉取客户端失败");
        Problem::new(ErrorCode::Internal)
    })?;
    let response = client.get(target.clone()).send().await.map_err(|error| {
        tracing::debug!(host = %target.host_str().unwrap_or_default(), error = %error, "拉取远端书源失败");
        Problem::new(ErrorCode::UpstreamFailed).detail("远端书源请求失败")
    })?;
    if !response.status().is_success() {
        return Err(Problem::new(ErrorCode::UpstreamFailed)
            .detail(format!("远端书源返回 HTTP {}", response.status().as_u16())));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return Err(Problem::new(ErrorCode::PayloadTooLarge).detail("远端书源响应不能超过 32MB"));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            tracing::debug!(error = %error, "读取远端书源响应失败");
            Problem::new(ErrorCode::UpstreamFailed).detail("远端书源响应读取失败")
        })?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES as usize {
            return Err(
                Problem::new(ErrorCode::PayloadTooLarge).detail("远端书源响应不能超过 32MB")
            );
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(Json(parse_sources_text(&text)?))
}

/// 全量导出书源材料（含全部规则字段），带 `Content-Disposition: attachment`。
#[utoipa::path(
    get,
    path = "/v1/sources:export",
    tag = "sources",
    operation_id = "exportSources",
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "全量书源 JSON 数组（attachment）", body = [BookSource]),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn export_sources(
    State(state): State<AppState>,
    V1User(user): V1User,
) -> Result<Response, Problem> {
    let sources = state
        .book_source_service
        .export_values(&user.ns)
        .await
        .map_err(Problem::from)?;
    let body = serde_json::to_vec(&sources).expect("sources serialize");
    let filename = format!(
        "reader-sources-{}.json",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    let disposition = format!("attachment; filename=\"{filename}\"");
    Ok((
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_string(),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        body,
    )
        .into_response())
}
