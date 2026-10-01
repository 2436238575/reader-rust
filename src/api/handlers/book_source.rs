use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::crawler::url_guard;
use crate::error::error::{ApiResponse, AppError};
use crate::model::book_source::{book_source_from_value, BookSource};
use crate::service::book_source_service::{
    book_source_has_group, set_invalid_book_source_group, INVALID_BOOK_SOURCE_GROUP,
};
use crate::util::text::{normalize_source_url, repair_encoded_url};
use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{
        header::{self, HeaderMap, HeaderValue},
        Method,
    },
    response::{IntoResponse, Response},
    Json,
};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, sync::Arc};

use once_cell::sync::Lazy;
use tokio::{sync::Semaphore, task::JoinSet};
use url::Url;

const MAX_TEST_SOURCE_BATCH_SIZE: usize = 100;

/// `readSourceFile` 上传的书源文件上限：书源是 JSON 文本，32MB 已远超正常体积。
const MAX_SOURCE_FILE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct BookSourceUrlParam {
    #[serde(rename = "bookSourceUrl")]
    book_source_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExploreKindsRequest {
    #[serde(rename = "bookSourceUrl")]
    book_source_url: Option<String>,
    #[serde(rename = "bookSource")]
    book_source: Option<BookSource>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct TestBookSourcesRequest {
    pub book_source_urls: Option<Vec<String>>,
    pub keyword: Option<String>,
    pub mark_invalid: Option<bool>,
    pub concurrent: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestBookSourceItem {
    book_source_url: String,
    book_source_name: String,
    valid: bool,
    search_ok: bool,
    explore_ok: bool,
    keyword: String,
    explore_url: Option<String>,
    search_error: Option<String>,
    explore_error: Option<String>,
    marked_invalid: bool,
    group: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestBookSourcesResponse {
    total: usize,
    valid: usize,
    invalid: usize,
    marked_invalid: usize,
    results: Vec<TestBookSourceItem>,
}

pub async fn save_book_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let source =
        book_source_from_value(payload).map_err(|e| AppError::BadRequest(e.to_string()))?;
    state.book_source_service.save(&user_ns, source).await?;
    Ok(Json(ApiResponse::ok(serde_json::json!({"saved": true}))))
}

pub async fn save_book_sources(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let sources = extract_sources(payload)?;
    if sources.is_empty() {
        return Err(AppError::BadRequest("empty book sources".to_string()));
    }
    let count = sources.len();
    state
        .book_source_service
        .save_many(&user_ns, sources)
        .await?;
    Ok(Json(ApiResponse::ok(
        serde_json::json!({"saved": true, "count": count}),
    )))
}

pub async fn get_book_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<BookSourceUrlParam>,
    body: Option<Json<BookSourceUrlParam>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let url = q
        .book_source_url
        .or_else(|| body.and_then(|b| b.0.book_source_url));
    let url = url.ok_or_else(|| AppError::BadRequest("bookSourceUrl required".to_string()))?;
    let source = state
        .book_source_service
        .get(&user_ns, &url)
        .await?
        .ok_or_else(|| AppError::NotFound("bookSource not found".to_string()))?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(source).unwrap_or_default(),
    )))
}

pub async fn get_book_sources(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let list = state.book_source_service.list(&user_ns).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(list).unwrap_or_default(),
    )))
}

pub async fn login_book_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(param): Json<BookSourceUrlParam>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let url = param
        .book_source_url
        .ok_or_else(|| AppError::BadRequest("bookSourceUrl required".to_string()))?;
    let source = state
        .book_source_service
        .get(&user_ns, &url)
        .await?
        .ok_or_else(|| AppError::NotFound("bookSource not found".to_string()))?;
    let result = state
        .book_service
        .login_book_source(&user_ns, &source)
        .await?;
    Ok(Json(ApiResponse::ok(result)))
}

pub async fn get_explore_kinds(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<ExploreKindsRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let source = if let Some(source) = req.book_source {
        source
    } else {
        let url = req
            .book_source_url
            .ok_or_else(|| AppError::BadRequest("bookSourceUrl required".to_string()))?;
        state
            .book_source_service
            .get(&user_ns, &url)
            .await?
            .ok_or_else(|| AppError::NotFound("bookSource not found".to_string()))?
    };

    let kinds = state.book_service.explore_kinds(&user_ns, &source).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(kinds).unwrap_or_default(),
    )))
}

pub async fn test_book_sources(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<TestBookSourcesRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let requested = normalize_requested_source_urls(req.book_source_urls.as_deref())?;
    let sources = state
        .book_source_service
        .list(&user_ns)
        .await?
        .into_iter()
        .filter(|source| {
            requested
                .as_ref()
                .map(|urls| urls.contains(&normalize_source_url(&source.book_source_url)))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();

    let concurrent = req.concurrent.unwrap_or(12).clamp(1, 12);
    let keyword = req.keyword.clone();
    let mark_invalid = req.mark_invalid.unwrap_or(true);
    let outcomes = test_sources_in_parallel(
        state.book_service.clone(),
        user_ns.clone(),
        keyword,
        sources,
        concurrent,
    )
    .await;

    let mut results = Vec::with_capacity(outcomes.len());
    let mut marked_invalid = 0usize;
    for (mut source, availability) in outcomes {
        let changed = if mark_invalid {
            set_invalid_book_source_group(&mut source, !availability.valid)
        } else {
            false
        };
        if changed {
            state
                .book_source_service
                .save(&user_ns, source.clone())
                .await?;
            if !availability.valid {
                marked_invalid += 1;
            }
        }

        results.push(TestBookSourceItem {
            book_source_url: availability.book_source_url,
            book_source_name: availability.book_source_name,
            valid: availability.valid,
            search_ok: availability.search_ok,
            explore_ok: availability.explore_ok,
            keyword: availability.keyword,
            explore_url: availability.explore_url,
            search_error: availability.search_error,
            explore_error: availability.explore_error,
            marked_invalid: changed && !availability.valid,
            group: source.book_source_group,
        });
    }

    results.sort_by(|a, b| a.book_source_name.cmp(&b.book_source_name));
    let valid = results.iter().filter(|item| item.valid).count();
    let invalid = results.len().saturating_sub(valid);
    let response = TestBookSourcesResponse {
        total: results.len(),
        valid,
        invalid,
        marked_invalid,
        results,
    };
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(response).unwrap_or_default(),
    )))
}

fn normalize_requested_source_urls(
    urls: Option<&[String]>,
) -> Result<Option<HashSet<String>>, AppError> {
    let Some(urls) = urls else {
        return Ok(None);
    };
    if urls.len() > MAX_TEST_SOURCE_BATCH_SIZE {
        return Err(AppError::BadRequest(format!(
            "bookSourceUrls 最多支持 {} 条",
            MAX_TEST_SOURCE_BATCH_SIZE
        )));
    }
    Ok(Some(
        urls.iter()
            .map(|url| normalize_source_url(url))
            .filter(|url| !url.is_empty())
            .collect::<HashSet<_>>(),
    ))
}

async fn test_sources_in_parallel(
    book_service: Arc<crate::service::book_service::BookService>,
    user_ns: String,
    keyword: Option<String>,
    sources: Vec<BookSource>,
    concurrent: usize,
) -> Vec<(
    BookSource,
    crate::service::book_service::BookSourceAvailability,
)> {
    let permits = Arc::new(Semaphore::new(concurrent.max(1)));
    let mut tasks = JoinSet::new();

    for source in sources {
        let permit = match permits.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };
        let book_service = book_service.clone();
        let user_ns = user_ns.clone();
        let keyword = keyword.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let availability = book_service
                .test_book_source_availability(&user_ns, &source, keyword.as_deref())
                .await;
            (source, availability)
        });
    }

    let mut outcomes = Vec::with_capacity(tasks.len());
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(outcome) => outcomes.push(outcome),
            Err(err) => tracing::error!("book source test task failed: {err}"),
        }
    }
    outcomes
}

pub async fn delete_invalid_book_sources(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let sources = state.book_source_service.list(&user_ns).await?;
    let invalid_urls = sources
        .iter()
        .filter(|source| book_source_has_group(source, INVALID_BOOK_SOURCE_GROUP))
        .map(|source| source.book_source_url.clone())
        .collect::<Vec<_>>();
    for url in &invalid_urls {
        state.book_source_service.delete(&user_ns, url).await?;
    }
    Ok(Json(ApiResponse::ok(serde_json::json!({
        "deleted": invalid_urls.len()
    }))))
}

#[derive(Debug, Deserialize)]
pub struct BookSourceProxyParam {
    #[serde(rename = "bookSourceUrl")]
    book_source_url: Option<String>,
    url: Option<String>,
}

pub async fn book_source_proxy(
    State(state): State<AppState>,
    user: CurrentUser,
    method: Method,
    headers: HeaderMap,
    Query(q): Query<BookSourceProxyParam>,
    body: Bytes,
) -> Result<Response, AppError> {
    let user_ns = user.0.ns.clone();

    let source_url = q
        .book_source_url
        .ok_or_else(|| AppError::BadRequest("bookSourceUrl required".to_string()))?;
    let raw_target_url = q
        .url
        .ok_or_else(|| AppError::BadRequest("url required".to_string()))?;

    let source = state
        .book_source_service
        .get(&user_ns, &source_url)
        .await?
        .ok_or_else(|| AppError::NotFound("bookSource not found".to_string()))?;

    if let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        state
            .book_service
            .set_source_cookie(&user_ns, &source.book_source_url, cookie)
            .await;
    }

    let target_url = resolve_proxy_target_url(&raw_target_url, &source.book_source_url)?;
    // 出站守卫：代理目标由查询参数决定，可能指向内网/云元数据地址
    url_guard::ensure_outbound_url_str_allowed(&target_url)
        .await
        .map_err(AppError::BadRequest)?;
    let upstream_referer = extract_upstream_referer(&headers);
    // 限定令牌只能继续代理它绑定的那个书源
    if let Some(bound) = user.0.proxy_source.as_deref() {
        if bound != source.book_source_url {
            return Err(AppError::BadRequest("代理令牌与书源不匹配".to_string()));
        }
    }
    // 被代理页面是第三方书源的 HTML/JS，与主站同源吐出：嵌进去的必须是
    // 限定令牌（短寿命、绑定书源、只能访问代理路径），而不是主 JWT。
    let page_token = mint_proxy_token(&state, &user.0, &source.book_source_url)?;
    let response = forward_book_source_request(
        &state,
        &source,
        &user_ns,
        Some(page_token.as_str()),
        &method,
        &headers,
        &target_url,
        upstream_referer.as_deref(),
        body,
    )
    .await?;

    Ok(response)
}

/// 代理令牌有效期：每次页面响应都会重签一把新的，30 分钟足够完成一次登录流程。
const PROXY_TOKEN_TTL_SECS: i64 = 30 * 60;

fn mint_proxy_token(
    state: &AppState,
    user: &crate::auth::extractor::AuthUser,
    book_source_url: &str,
) -> Result<String, AppError> {
    let now = chrono::Utc::now().timestamp();
    let claims = crate::auth::jwt::Claims {
        sub: user.username.clone(),
        ns: user.ns.clone(),
        iat: now,
        exp: now + PROXY_TOKEN_TTL_SECS,
        ver: user.ver,
        scope: Some(crate::auth::jwt::PROXY_TOKEN_SCOPE.to_string()),
        bsu: Some(book_source_url.to_string()),
    };
    crate::auth::jwt::encode_token(&claims, state.auth.jwt_secret())
}

#[derive(Debug, Deserialize)]
pub struct BookSourceClientLogParam {
    message: Option<String>,
    source: Option<String>,
    lineno: Option<i64>,
    colno: Option<i64>,
    stack: Option<String>,
}

/// 客户端日志单字段的截断长度。
const LOG_FIELD_MAX_CHARS: usize = 500;
/// 客户端日志限频：同一 source 每分钟最多入库的条数。
const CLIENT_LOG_MAX_PER_MINUTE: usize = 30;

static CLIENT_LOG_BUCKETS: Lazy<
    std::sync::Mutex<
        std::collections::HashMap<String, std::collections::VecDeque<std::time::Instant>>,
    >,
> = Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn allow_client_log(source: &str) -> bool {
    let mut map = CLIENT_LOG_BUCKETS.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    let bucket = map.entry(source.to_string()).or_default();
    bucket.retain(|t| now.duration_since(*t) < std::time::Duration::from_secs(60));
    if bucket.len() >= CLIENT_LOG_MAX_PER_MINUTE {
        return false;
    }
    bucket.push_back(now);
    true
}

pub async fn book_source_client_log(
    Query(q): Query<BookSourceClientLogParam>,
) -> Json<ApiResponse<serde_json::Value>> {
    // 上报内容完全不可信：截断后再落日志；同一 source 超频的整条丢弃，
    // 防止登录用户刷 warn 日志把日志文件撑爆
    let allow = allow_client_log(q.source.as_deref().unwrap_or_default());
    if allow {
        let clip = |value: Option<&str>| {
            value
                .unwrap_or_default()
                .chars()
                .take(LOG_FIELD_MAX_CHARS)
                .collect::<String>()
        };
        tracing::warn!(
            "bookSourceProxy client error: source={} line={} col={} message={} stack={}",
            clip(q.source.as_deref()),
            q.lineno.unwrap_or_default(),
            q.colno.unwrap_or_default(),
            clip(q.message.as_deref()),
            clip(q.stack.as_deref())
        );
    }
    Json(ApiResponse::ok(serde_json::json!({ "logged": allow })))
}

pub async fn delete_book_source(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(param): Json<BookSourceUrlParam>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let url = param
        .book_source_url
        .ok_or_else(|| AppError::BadRequest("bookSourceUrl required".to_string()))?;
    state.book_source_service.delete(&user_ns, &url).await?;
    Ok(Json(ApiResponse::ok(serde_json::json!({"deleted": true}))))
}

fn resolve_proxy_target_url(
    raw_target_url: &str,
    book_source_url: &str,
) -> Result<String, AppError> {
    let repaired = repair_encoded_url(raw_target_url);
    if let Ok(url) = Url::parse(&repaired) {
        return Ok(url.to_string());
    }

    let base = normalize_source_url(book_source_url);
    let base = Url::parse(&base)
        .map_err(|e| AppError::BadRequest(format!("invalid bookSourceUrl: {}", e)))?;
    base.join(&repaired)
        .map(|u| u.to_string())
        .map_err(|e| AppError::BadRequest(format!("invalid proxy target url: {}", e)))
}

fn extract_upstream_referer(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::REFERER)?.to_str().ok()?;
    let referer = Url::parse(raw).ok()?;
    let params: std::collections::HashMap<String, String> =
        referer.query_pairs().into_owned().collect();
    params.get("url").cloned().map(|v| repair_encoded_url(&v))
}

async fn forward_book_source_request(
    state: &AppState,
    source: &BookSource,
    user_ns: &str,
    access_token: Option<&str>,
    method: &Method,
    headers: &HeaderMap,
    target_url: &str,
    upstream_referer: Option<&str>,
    body: Bytes,
) -> Result<Response, AppError> {
    let client = state
        .book_service
        .http_client(user_ns)
        .map_err(AppError::Internal)?;
    let req_method = match *method {
        Method::GET => reqwest::Method::GET,
        Method::POST => reqwest::Method::POST,
        _ => return Err(AppError::BadRequest("unsupported proxy method".to_string())),
    };

    let mut builder = client.request(req_method, target_url);

    if let Some(header_str) = &source.header {
        if let Ok(source_headers) =
            serde_json::from_str::<std::collections::HashMap<String, String>>(header_str)
        {
            for (k, v) in source_headers {
                builder = builder.header(k, v);
            }
        }
    }

    let mut has_content_type = false;
    let mut has_x_requested_with = false;
    for (name, value) in headers.iter() {
        if should_forward_request_header(name.as_str()) {
            if name.as_str().eq_ignore_ascii_case("content-type") {
                has_content_type = true;
            }
            if name.as_str().eq_ignore_ascii_case("x-requested-with") {
                has_x_requested_with = true;
            }
            builder = builder.header(name, value.clone());
        }
    }

    if let Some(cookie) = headers.get(header::COOKIE) {
        builder = builder.header(header::COOKIE, cookie.clone());
    }

    let referer_value = upstream_referer.unwrap_or(target_url);
    builder = builder.header(header::REFERER, referer_value);
    if let Ok(url) = Url::parse(referer_value) {
        let origin = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
        builder = builder.header(header::ORIGIN, origin);
    }

    if method == Method::POST && !has_content_type {
        builder = builder.header(
            header::CONTENT_TYPE,
            "application/x-www-form-urlencoded; charset=UTF-8",
        );
    }
    if is_ajax_api_target(target_url) && !has_x_requested_with {
        builder = builder.header("X-Requested-With", "XMLHttpRequest");
    }

    if method == Method::POST {
        builder = builder.body(body.clone());
    }

    tracing::info!(
        "bookSourceProxy upstream request: method={} target={} referer={} body_len={}",
        method,
        redact_url_query(target_url),
        redact_url_query(referer_value),
        body.len()
    );
    let upstream = builder.send().await.map_err(AppError::Http)?;
    let status = upstream.status();
    let final_url = upstream.url().to_string();
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let upstream_set_cookies: Vec<String> = upstream
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(|s| s.to_string()))
        .collect();
    let bytes = crate::crawler::fetcher::read_body_limited(
        upstream,
        crate::crawler::fetcher::MAX_RESPONSE_BYTES,
    )
    .await
    .map_err(|e| AppError::BadRequest(e.to_string()))?;
    tracing::info!(
        "bookSourceProxy upstream response: method={} target={} status={} final_url={}",
        method,
        redact_url_query(target_url),
        status,
        redact_url_query(&final_url)
    );
    // 响应预览可能含登录态/签名等敏感内容，只进 debug 日志
    if is_ajax_api_target(target_url) {
        let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(500)]).replace('\n', "\\n");
        tracing::debug!(
            "bookSourceProxy upstream api body: target={} status={} preview={}",
            redact_url_query(target_url),
            status,
            preview
        );
    }
    if status.is_client_error() || status.is_server_error() {
        let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(400)]).replace('\n', "\\n");
        tracing::debug!(
            "bookSourceProxy upstream error body: target={} status={} preview={}",
            redact_url_query(target_url),
            status,
            preview
        );
    }

    let mut response_headers = HeaderMap::new();
    if let Some(ct) = content_type.as_deref() {
        if let Ok(value) = HeaderValue::from_str(ct) {
            response_headers.insert(header::CONTENT_TYPE, value);
        }
    }
    for cookie in upstream_set_cookies {
        if let Some(rewritten) = rewrite_set_cookie_for_proxy(&cookie) {
            if let Ok(value) = HeaderValue::from_str(&rewritten) {
                response_headers.append(header::SET_COOKIE, value);
            }
        }
    }
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

    let body = if is_html_response(content_type.as_deref(), &bytes) {
        let text = String::from_utf8_lossy(&bytes).to_string();
        rewrite_login_html(&text, &final_url, &source.book_source_url, access_token).into_bytes()
    } else {
        bytes.to_vec()
    };

    Ok((status, response_headers, body).into_response())
}

fn should_forward_request_header(name: &str) -> bool {
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "authorization" | "referer" | "origin" | "connection"
    )
}

fn is_ajax_api_target(target_url: &str) -> bool {
    if let Ok(url) = Url::parse(target_url) {
        let path = url.path().to_ascii_lowercase();
        return path.ends_with("_api") || path.contains("/api/");
    }
    false
}

/// 日志里的上游地址一律剥掉查询串：书源站点的签名/登录态参数不该落进日志文件。
fn redact_url_query(raw: &str) -> String {
    url_guard::redact_url_for_log(raw)
}

fn is_html_response(content_type: Option<&str>, body: &[u8]) -> bool {
    if let Some(ct) = content_type {
        if ct.to_ascii_lowercase().contains("text/html") {
            return true;
        }
    }
    let prefix = String::from_utf8_lossy(&body[..body.len().min(256)]).to_ascii_lowercase();
    prefix.contains("<html") || prefix.contains("<!doctype html")
}

fn rewrite_login_html(
    html: &str,
    upstream_url: &str,
    book_source_url: &str,
    access_token: Option<&str>,
) -> String {
    let base_href = html_escape_attr(upstream_url);
    let proxy_script = build_proxy_script(upstream_url, book_source_url, access_token);
    let mut output = if html.contains("<head") {
        html.replace(
            "</head>",
            &format!(r#"<base href="{base_href}">{proxy_script}</head>"#),
        )
    } else {
        format!(
            r#"<!DOCTYPE html><html><head><base href="{base_href}">{proxy_script}</head><body>{html}</body></html>"#
        )
    };

    output = rewrite_proxy_actions(&output, upstream_url, book_source_url, access_token);
    output =
        rewrite_script_root_relative_urls(&output, upstream_url, book_source_url, access_token);
    output
}

fn build_proxy_script(
    upstream_url: &str,
    book_source_url: &str,
    access_token: Option<&str>,
) -> String {
    let upstream_json = serde_json::to_string(upstream_url).unwrap_or_else(|_| "\"\"".to_string());
    let source_json = serde_json::to_string(book_source_url).unwrap_or_else(|_| "\"\"".to_string());
    let token_json =
        serde_json::to_string(access_token.unwrap_or("")).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"<script>
(function() {{
  const upstreamBase = {upstream_json};
  const bookSourceUrl = {source_json};
  const accessToken = {token_json};
  const proxyPath = "/reader3/bookSourceProxy";
  const alreadyProxyPattern = /^\/reader3\/bookSourceProxy(?:\?|$)/i;
  const skipPattern = /^(#|javascript:|data:|mailto:|tel:)/i;
  function toAbsolute(url) {{
    try {{ return new URL(url, upstreamBase).href; }} catch (_e) {{ return url; }}
  }}
  function toProxy(url) {{
    if (!url || skipPattern.test(url) || alreadyProxyPattern.test(url)) return url;
    const absolute = toAbsolute(url);
    if (String(absolute).indexOf("/reader3/bookSourceProxy?") !== -1) return absolute;
    const params = new URLSearchParams();
    if (accessToken) params.set("accessToken", accessToken);
    params.set("bookSourceUrl", bookSourceUrl);
    params.set("url", absolute);
    return proxyPath + "?" + params.toString();
  }}
  window.__readerBookSourceProxy = {{ toProxy, upstreamBase }};
  const rawFetch = window.fetch ? window.fetch.bind(window) : null;
  if (rawFetch) {{
    window.fetch = function(input, init) {{
      try {{
        if (input instanceof Request) {{
          return rawFetch(new Request(toProxy(input.url), input), init);
        }}
        return rawFetch(toProxy(String(input)), init);
      }} catch (_e) {{
        return rawFetch(input, init);
      }}
    }};
  }}
  const rawOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function(method, url) {{
    arguments[1] = toProxy(String(url));
    return rawOpen.apply(this, arguments);
  }};
  document.addEventListener("submit", function(event) {{
    const form = event.target;
    if (!(form instanceof HTMLFormElement)) return;
    const action = form.getAttribute("action") || upstreamBase;
    form.setAttribute("action", toProxy(action));
  }}, true);
  document.addEventListener("click", function(event) {{
    const anchor = event.target && event.target.closest ? event.target.closest("a[href]") : null;
    if (!anchor) return;
    const href = anchor.getAttribute("href");
    if (!href || skipPattern.test(href)) return;
    anchor.setAttribute("href", toProxy(href));
  }}, true);
  function reportClientError(payload) {{
    try {{
      const params = new URLSearchParams();
      Object.entries(payload || {{}}).forEach(function(entry) {{
        const key = entry[0];
        const value = entry[1];
        if (value !== undefined && value !== null && value !== "") {{
          params.set(key, String(value));
        }}
      }});
      const url = "/reader3/bookSourceClientLog?" + params.toString();
      if (navigator.sendBeacon) {{
        navigator.sendBeacon(url);
      }} else if (rawFetch) {{
        rawFetch(url, {{ method: "POST" }});
      }}
    }} catch (_e) {{}}
  }}
  window.addEventListener("error", function(event) {{
    reportClientError({{
      message: event.message || "window error",
      source: event.filename || "",
      lineno: event.lineno || 0,
      colno: event.colno || 0,
      stack: event.error && event.error.stack ? event.error.stack : ""
    }});
  }});
  window.addEventListener("unhandledrejection", function(event) {{
    const reason = event.reason;
    reportClientError({{
      message: reason && reason.message ? reason.message : String(reason || "unhandled rejection"),
      stack: reason && reason.stack ? reason.stack : ""
    }});
  }});
}})();
</script>"#
    )
}

// 代理页 HTML 重写的固定正则：每次请求现编译太浪费，全部只编译一次。
static TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<[^>]+>"#).unwrap());
static DOUBLE_QUOTED_ATTR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?i)\b(action|href|src)\s*=\s*"([^"]+)""#).unwrap());
static SINGLE_QUOTED_ATTR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?i)\b(action|href|src)\s*=\s*'([^']+)'"#).unwrap());
static SCRIPT_BLOCK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?is)<script\b[^>]*>.*?</script>"#).unwrap());
static DOUBLE_QUOTED_ROOT_REL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#""(/[^"\\\s<]*)""#).unwrap());
static SINGLE_QUOTED_ROOT_REL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"'(/[^'\\\s<]*)'"#).unwrap());

fn rewrite_proxy_actions(
    html: &str,
    upstream_url: &str,
    book_source_url: &str,
    access_token: Option<&str>,
) -> String {
    let tag_re = &*TAG_RE;
    let double_quoted = &*DOUBLE_QUOTED_ATTR_RE;
    let single_quoted = &*SINGLE_QUOTED_ATTR_RE;

    tag_re
        .replace_all(html, |tag_caps: &Captures| {
            let tag = tag_caps.get(0).map(|m| m.as_str()).unwrap_or("");
            let output = double_quoted.replace_all(tag, |caps: &Captures| {
                rewrite_proxy_attr(caps, upstream_url, book_source_url, access_token, "\"")
            });
            single_quoted
                .replace_all(&output, |caps: &Captures| {
                    rewrite_proxy_attr(caps, upstream_url, book_source_url, access_token, "'")
                })
                .into_owned()
        })
        .into_owned()
}

fn rewrite_proxy_attr(
    caps: &Captures,
    upstream_url: &str,
    book_source_url: &str,
    access_token: Option<&str>,
    quote: &str,
) -> String {
    let attr = caps.get(1).map(|m| m.as_str()).unwrap_or("href");
    let value = caps.get(2).map(|m| m.as_str()).unwrap_or("");
    let proxied = build_proxy_url(value, upstream_url, book_source_url, access_token)
        .unwrap_or_else(|| value.to_string());
    format!(r#"{attr}={quote}{proxied}{quote}"#)
}

fn build_proxy_url(
    raw_value: &str,
    upstream_url: &str,
    book_source_url: &str,
    access_token: Option<&str>,
) -> Option<String> {
    let trimmed = raw_value.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('#')
        || trimmed.starts_with("javascript:")
        || trimmed.starts_with("data:")
        || trimmed.starts_with("mailto:")
        || trimmed.starts_with("tel:")
        || trimmed.starts_with("/reader3/bookSourceProxy")
    {
        return None;
    }

    let absolute = Url::parse(trimmed)
        .or_else(|_| Url::parse(upstream_url).and_then(|base| base.join(trimmed)))
        .ok()?;
    let mut params = vec![
        format!("bookSourceUrl={}", urlencoding::encode(book_source_url)),
        format!("url={}", urlencoding::encode(absolute.as_str())),
    ];
    if let Some(token) = access_token.filter(|v| !v.is_empty()) {
        params.push(format!("accessToken={}", urlencoding::encode(token)));
    }
    Some(format!("/reader3/bookSourceProxy?{}", params.join("&")))
}

fn rewrite_script_root_relative_urls(
    html: &str,
    upstream_url: &str,
    book_source_url: &str,
    access_token: Option<&str>,
) -> String {
    let script_re = &*SCRIPT_BLOCK_RE;
    let double_quoted = &*DOUBLE_QUOTED_ROOT_REL_RE;
    let single_quoted = &*SINGLE_QUOTED_ROOT_REL_RE;

    script_re
        .replace_all(html, |script_caps: &Captures| {
            let script = script_caps.get(0).map(|m| m.as_str()).unwrap_or("");
            let output = double_quoted.replace_all(script, |caps: &Captures| {
                rewrite_script_url_literal(caps, upstream_url, book_source_url, access_token, "\"")
            });
            single_quoted
                .replace_all(&output, |caps: &Captures| {
                    rewrite_script_url_literal(
                        caps,
                        upstream_url,
                        book_source_url,
                        access_token,
                        "'",
                    )
                })
                .into_owned()
        })
        .into_owned()
}

fn rewrite_script_url_literal(
    caps: &Captures,
    upstream_url: &str,
    book_source_url: &str,
    access_token: Option<&str>,
    quote: &str,
) -> String {
    let raw_value = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let proxied = build_proxy_url(raw_value, upstream_url, book_source_url, access_token)
        .unwrap_or_else(|| raw_value.to_string());
    format!("{quote}{proxied}{quote}")
}

fn html_escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn rewrite_set_cookie_for_proxy(raw: &str) -> Option<String> {
    let mut parts = raw
        .split(';')
        .map(|part| part.trim())
        .filter(|part| !part.is_empty());
    let first = parts.next()?;
    if !first.contains('=') {
        return None;
    }

    let mut attrs = vec![
        first.to_string(),
        "Path=/reader3/bookSourceProxy".to_string(),
    ];
    for attr in parts {
        let lower = attr.to_ascii_lowercase();
        if lower.starts_with("domain=") || lower.starts_with("path=") || lower == "secure" {
            continue;
        }
        attrs.push(attr.to_string());
    }
    Some(attrs.join("; "))
}

pub async fn delete_book_sources(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(list): Json<Vec<BookSourceUrlParam>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    for item in list {
        if let Some(url) = item.book_source_url {
            state.book_source_service.delete(&user_ns, &url).await?;
        }
    }
    Ok(Json(ApiResponse::ok(serde_json::json!({"deleted": true}))))
}

pub async fn delete_all_book_sources(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    state.book_source_service.delete_all(&user_ns).await?;
    Ok(Json(ApiResponse::ok(serde_json::json!({"deleted": true}))))
}

fn extract_sources(payload: serde_json::Value) -> Result<Vec<BookSource>, AppError> {
    if let Some(items) = payload.as_array() {
        return items
            .iter()
            .cloned()
            .map(|value| {
                book_source_from_value(value).map_err(|e| AppError::BadRequest(e.to_string()))
            })
            .collect();
    }
    if let Some(obj) = payload.as_object() {
        for key in ["bookSourceList", "bookSources", "data", "sources"] {
            if let Some(v) = obj.get(key) {
                if let Some(items) = v.as_array() {
                    return items
                        .iter()
                        .cloned()
                        .map(|value| {
                            book_source_from_value(value)
                                .map_err(|e| AppError::BadRequest(e.to_string()))
                        })
                        .collect();
                }
            }
        }
    }
    Err(AppError::BadRequest(
        "invalid book sources payload".to_string(),
    ))
}

#[derive(Debug, Deserialize)]
pub struct RemoteSourceParam {
    url: String,
}

pub async fn read_remote_source_file(
    Json(param): Json<RemoteSourceParam>,
) -> Result<Json<ApiResponse<Vec<String>>>, AppError> {
    // 出站守卫：url 完全由调用方提供，必须挡掉内网目标
    let target = url_guard::ensure_outbound_url_str_allowed(&param.url)
        .await
        .map_err(AppError::BadRequest)?;
    let mut builder = url_guard::guarded_client_builder(
        std::time::Duration::from_secs(30),
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    );
    if url_guard::private_network_allowed() {
        // 仅显式放行私网（ALLOW_PRIVATE_NETWORK=true）时容忍自签证书（内网书源常见）
        builder = builder.danger_accept_invalid_certs(true);
    }
    let client = builder.build().map_err(|e| AppError::Internal(e.into()))?;

    let response = client.get(target).send().await.map_err(|e| {
        tracing::error!("Failed to fetch remote source from {}: {:?}", param.url, e);
        AppError::BadRequest(format!("网络请求失败: {}", e))
    })?;
    // 响应体限量：上游可返回超大响应把进程内存打爆（与 RSS 抓取同规格）
    let bytes = crate::crawler::fetcher::read_body_limited(
        response,
        crate::crawler::fetcher::MAX_RESPONSE_BYTES,
    )
    .await
    .map_err(|e| AppError::BadRequest(format!("读取响应失败: {}", e)))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();

    tracing::debug!(length = text.len(), "remote source file fetched");
    tracing::trace!(
        preview = %&text.chars().take(500).collect::<String>(),
        "remote source file preview"
    );

    let sources: Vec<BookSource> = serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|_| AppError::BadRequest("invalid book sources json format".to_string()))
        .and_then(extract_sources)?;

    tracing::debug!(count = sources.len(), "parsed remote book sources");

    // Return as array of JSON strings (frontend expects each item to be a JSON string)
    let json_str = serde_json::to_string(&sources)
        .map_err(|e| AppError::BadRequest(format!("序列化书源失败: {}", e)))?;

    Ok(Json(ApiResponse::ok(vec![json_str])))
}

use axum::extract::Multipart;

pub async fn read_source_file(
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        if let Some(file_name) = field.file_name() {
            if file_name.ends_with(".json") || file_name.ends_with(".txt") {
                // DefaultBodyLimit 不覆盖 multipart 字段，必须逐块限量读
                let bytes = super::multipart::read_limited_multipart_field(
                    field,
                    MAX_SOURCE_FILE_BYTES,
                    "书源文件过大（上限 32MB）",
                )
                .await?;
                let text = String::from_utf8_lossy(&bytes);
                let sources: Vec<BookSource> = serde_json::from_str::<serde_json::Value>(&text)
                    .map_err(|_| {
                        AppError::BadRequest("invalid book sources json format".to_string())
                    })
                    .and_then(extract_sources)?;
                return Ok(Json(serde_json::to_value(sources).unwrap_or_default()));
            }
        }
    }
    Err(AppError::BadRequest("No json file uploaded".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_source_urls_rejects_batches_above_limit() {
        let urls = (0..101)
            .map(|index| format!("https://source-{index}.example"))
            .collect::<Vec<_>>();

        let err = normalize_requested_source_urls(Some(&urls)).unwrap_err();

        assert!(matches!(err, AppError::BadRequest(message) if message.contains("100")));
    }
}
