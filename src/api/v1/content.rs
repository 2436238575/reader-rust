//! `GET /v1/books/{bookId}/chapters/{index}/content`：章节正文。
//!
//! 与 `/reader3/getBookContent` 共用同一份读取链路（版本解析、强刷、24h
//! 后台回源、本地书分支）；差异在对外契约：
//! - **身份是 `(bookId, index)`**：index 经章节登记表/本地目录解析出内部
//!   chapterUrl（红线 6）；
//! - **`If-None-Match` 取代 `knownVersionId`**：强 ETag 取**响应体的 md5**，
//!   命中回 304，客户端用本地缓存正文补上——浏览器/Service Worker 缓存
//!   自动生效。不能只拿 `cv_<version_id>` 当 ETag：diff / diffBaseVersionId /
//!   upstreamChangedSinceBase / lastAttemptAt / stale 这些字段不由当前版本
//!   决定（上游推进、后台回源尝试都会改），只盖版本号会让编辑视图永久
//!   拿缓存里的旧 diff；
//! - **`format` 由服务端统一判定**（D15）：沿用前端嗅探口径（块级标签
//!   白名单 + 成对 b/i/u 补偿），各客户端不再各猜一遍。
//!
//! Draft-1 §9.5 的 `?include=blocks` 暂未实现：blocks 的消费方（段评/编辑器）
//! 在 P3；内联 diff 已随用户版本一起下发。

use crate::api::etag::if_none_match_matches;
use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/books/{bookId}/chapters/{index}/content",
            get(get_chapter_content),
        )
        .route(
            "/v1/books/{bookId}/chapters:batchContent",
            post(batch_chapter_content),
        )
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ContentQuery {
    /// `default`（用户选择 > 最新用户版本 > 最新上游）或 `upstream`（显式上游预览）
    pub variant: Option<String>,
    /// 同步回源一次；失败回退已有缓存
    pub refresh: Option<bool>,
}

/// 当前解析版本的信息。`basedOn` 只记录编辑时基于哪一版（历史信息）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterVersionInfo {
    /// 不透明版本 id（`cv_<n>`），同时是 If-None-Match 的实体标签
    pub id: String,
    /// `upstream` / `edited` / `imported`
    pub kind: String,
    pub based_on: Option<String>,
}

/// 章节正文（v1 形状）。`format` 由服务端统一判定并下发（契约 §9.5）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterContent {
    pub book_id: String,
    pub index: i32,
    /// `text` 或 `html`（服务端统一判定，D15）
    pub format: String,
    pub body: String,
    pub version: Option<ChapterVersionInfo>,
    /// 当前返回的是用户版本（编辑版/导入版）
    pub edited: bool,
    pub latest_upstream_version_id: Option<String>,
    /// 本次 diff 的基准版本（始终是最新上游）
    pub diff_base_version_id: Option<String>,
    pub latest_user_version_id: Option<String>,
    /// 内联 diff（当前用户版本 vs 最新上游）：阅读器编辑视图消费；
    /// 上游预览与无用户版本时缺省
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub diff: Option<Vec<DiffOp>>,
    /// diff 算不出来的原因（正文仍可用）；diff 成功或没有用户版本时缺省
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub diff_error: Option<String>,
    /// 用户版本的基准上游之后，上游是否又变过
    pub upstream_changed_since_base: bool,
    /// `selected` / `edited` / `upstream` / `upstream_preview`
    pub selection_source: String,
    /// 到期未回源或最近一次回源失败（旧数据，仍可读）
    pub stale: bool,
    /// 最近一次回源成功时间，RFC 3339 UTC
    #[schema(format = DateTime)]
    pub fetched_at: Option<String>,
    /// 最近一次回源尝试时间（成功或失败），RFC 3339 UTC
    #[schema(format = DateTime)]
    pub last_attempt_at: Option<String>,
    /// 最新上游版本的出现时间，RFC 3339 UTC
    #[schema(format = DateTime)]
    pub upstream_changed_at: Option<String>,
    /// 当前用户版本的编辑时间，RFC 3339 UTC
    #[schema(format = DateTime)]
    pub edited_at: Option<String>,
}

use crate::service::chapter_diff_service::DiffOp;

fn cv_id(version_id: i64) -> String {
    format!("cv_{version_id}")
}

/// 正文格式判定（D15）：沿用前端 `chapterRender.ts` 的两段嗅探口径——
/// 块级/常用标签白名单 + 成对 b/i/u 补偿（regex crate 不支持反向引用，
/// 成对检测手工扫描）。
pub(crate) fn detect_content_format(body: &str) -> &'static str {
    static KNOWN_HTML_TAG: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(
            r"(?i)</?(?:p|div|br|hr|img|h[1-6]|ul|ol|li|dl|dt|dd|table|thead|tbody|tfoot|tr|td|th|caption|colgroup|col|blockquote|pre|code|figure|figcaption|section|article|aside|details|summary|center|span|em|strong|a|ruby|rt|rp|sup|sub|video|audio|svg)[\s/>]",
        )
        .expect("static regex")
    });
    if KNOWN_HTML_TAG.is_match(body) || has_paired_inline_tag(body) {
        "html"
    } else {
        "text"
    }
}

/// 成对 b/i/u 检测：存在开标签（可带属性）且其后有同名的闭合标签。
/// 纯文本里的 `<b>`、`<i>` 字面量没有闭合标签，不受影响。
fn has_paired_inline_tag(body: &str) -> bool {
    static OPEN_TAG: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"(?i)<(b|i|u)(?:\s[^<>]*)?>").expect("static regex")
    });
    for capture in OPEN_TAG.captures_iter(body) {
        let tag = capture
            .get(1)
            .map(|m| m.as_str())
            .expect("分组在模式里固定存在")
            .to_ascii_lowercase();
        let close_from = capture.get(0).expect("match 0 固定存在").end();
        let close = format!("</{tag}>");
        if body[close_from..].to_ascii_lowercase().contains(&close) {
            return true;
        }
    }
    false
}

/// index → 内部 chapterUrl：登记表/本地目录解析；URL 不出现在请求或响应里。
pub(crate) async fn resolve_chapter_url(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    book: &crate::api::handlers::book::ResolvedBook,
    index: i32,
) -> Result<String, Problem> {
    use crate::service::local_epub_book::is_local_epub_url;
    use crate::service::local_txt_book::is_local_txt_url;

    if is_local_txt_url(&book.book_url) || is_local_epub_url(&book.book_url) {
        let chapters = if is_local_txt_url(&book.book_url) {
            state
                .local_txt_book_service
                .get_chapter_list(user_ns, &book.book_url)
                .await
                .map_err(Problem::from)?
        } else {
            state
                .local_epub_book_service
                .get_chapter_list(user_ns, &book.book_url)
                .await
                .map_err(Problem::from)?
        };
        return chapters
            .get(index.max(0) as usize)
            .filter(|_| index >= 0)
            .map(|chapter| chapter.url.clone())
            .ok_or_else(chapter_not_found);
    }

    // 网络书：登记表是 index → URL 的权威映射（P-1）
    if let Some(chapter) = state
        .book_service
        .resolve_chapter_by_index(book_id, index)
        .await
        .map_err(Problem::from)?
    {
        return non_empty_url(chapter.url);
    }
    // 登记表已有条目：index 越界就是章节不存在——不回源目录，
    // 否则一次 404 会被上游故障放大成 400/502（404 的语义要稳定）
    if state
        .book_service
        .chapter_list_cache_exists(user_ns, book_id)
        .await
    {
        return Err(chapter_not_found());
    }
    // 登记表为空（目录从未抓过）：走一次目录抓取填充后再解析
    let toc_url = match book
        .toc_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(toc_url) => toc_url.to_string(),
        None => state
            .book_service
            .get_book_info(user_ns, book_id, &book.source, &book.book_url, false)
            .await
            .map_err(Problem::from)?
            .toc_url
            .unwrap_or_else(|| book.book_url.clone()),
    };
    let toc_url = crate::util::text::repair_encoded_url(&toc_url);
    // 抓取失败不外抛：目录拿不到时按章节不存在回答，而不是 502 掩盖 404
    let _ = state
        .book_service
        .get_chapter_list(user_ns, book_id, &book.source, &toc_url)
        .await;
    if let Some(chapter) = state
        .book_service
        .resolve_chapter_by_index(book_id, index)
        .await
        .map_err(Problem::from)?
    {
        return non_empty_url(chapter.url);
    }
    Err(chapter_not_found())
}

fn chapter_not_found() -> Problem {
    Problem::new(ErrorCode::ChapterNotFound).detail("章节不存在或没有可读地址")
}

fn non_empty_url(url: String) -> Result<String, Problem> {
    if url.trim().is_empty() {
        // 空 URL 章节（个别书源没有独立地址）在正文池里没有可定位的键
        return Err(chapter_not_found());
    }
    Ok(url)
}

/// 章节正文。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters/{index}/content",
    tag = "books",
    operation_id = "getChapterContent",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("index" = i32, Path, description = "章节序号（登记表分配的稳定序号）"),
        ("variant" = Option<String>, Query, description = "default（用户选择）或 upstream（上游预览）"),
        ("refresh" = Option<bool>, Query, description = "同步回源一次，失败回退已有缓存"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "章节正文", body = ChapterContent),
        (status = 304, description = "If-None-Match 命中当前版本，客户端用缓存正文"),
        (status = 400, description = "variant 取值非法", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍或章节不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_chapter_content(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path((book_id, index)): Path<(String, i32)>,
    Query(query): Query<ContentQuery>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    if let Some(variant) = query.variant.as_deref() {
        if !variant.trim().eq_ignore_ascii_case("default")
            && !variant.trim().eq_ignore_ascii_case("upstream")
        {
            return Err(Problem::new(ErrorCode::MalformedRequest)
                .detail("variant 只接受 default 或 upstream"));
        }
    }
    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;
    let chapter_url = resolve_chapter_url(&state, &user.ns, &book_id, &resolved, index).await?;

    let data = crate::api::handlers::book::content::read_chapter_content(
        &state,
        &user.ns,
        Some(&book_id),
        Some(&chapter_url),
        crate::api::handlers::book::content::ChapterReadOptions {
            index: Some(index),
            variant: crate::api::handlers::book::content::ContentVariant::parse(
                query.variant.as_deref(),
            ),
            do_refresh: query.refresh.unwrap_or(false),
            known_version_id: None,
        },
    )
    .await
    .map_err(Problem::from)?;

    // ETag 必须覆盖完整响应体：diff / diffBaseVersionId / upstreamChangedSinceBase /
    // lastAttemptAt / stale 都不由当前版本号决定（上游推进、后台回源尝试会改），
    // 只盖 `cv_<n>` 会让客户端永久拿缓存里的旧 diff。body 级 md5 一次算清。
    let content = map_to_v1(&book_id, index, data);
    let body = serde_json::to_vec(&content).expect("chapter content serializes");
    let etag = crate::api::etag::strong_etag(&body);
    let mut response = if if_none_match_matches(headers.get(header::IF_NONE_MATCH), &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(header::CONTENT_TYPE, "application/json")], body).into_response()
    };
    response.headers_mut().insert(header::ETAG, etag);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}

// ─── 批量正文（预加载用） ──────────────────────────────────────────────

/// 一次 1..=10 章，服务端并发 4，逐项成败：整体恒 200，每章 `data`/`error`
/// 二选一（契约 §8.4）。`?refresh=true` 同步回源后逐项读取。
#[utoipa::path(
    post,
    path = "/v1/books/{bookId}/chapters:batchContent",
    tag = "books",
    operation_id = "batchChapterContent",
    params(("bookId" = String, Path, description = "书籍 id（book_<n>）")),
    request_body = BatchContentRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "逐项结果", body = BatchContentResponse),
        (status = 400, description = "indices 为空/超上限/重复/含负数",
         body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn batch_chapter_content(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    Query(query): Query<BatchContentQuery>,
    V1Json(request): V1Json<BatchContentRequest>,
) -> Result<axum::Json<BatchContentResponse>, Problem> {
    if let Some(variant) = query.variant.as_deref() {
        if !variant.trim().eq_ignore_ascii_case("default")
            && !variant.trim().eq_ignore_ascii_case("upstream")
        {
            return Err(Problem::new(ErrorCode::MalformedRequest)
                .detail("variant 只接受 default 或 upstream"));
        }
    }
    let indices = &request.indices;
    if indices.is_empty() || indices.len() > 10 {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("indices 需要提供 1 到 10 个"));
    }
    if indices.iter().any(|index| *index < 0) {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("index 不能为负数"));
    }
    if indices
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != indices.len()
    {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("indices 不能重复"));
    }

    // 书籍存在性与书源解析只做一次；各章再按 index 单独解析
    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;

    let variant =
        crate::api::handlers::book::content::ContentVariant::parse(query.variant.as_deref());
    let do_refresh = query.refresh.unwrap_or(false);
    let indices = indices.clone();
    let mut results: Vec<(usize, BatchContentItem)> =
        futures::stream::iter(indices.into_iter().enumerate().map(|(position, index)| {
            let state = state.clone();
            let user_ns = user.ns.clone();
            let book_id = book_id.clone();
            let source = resolved.source.clone();
            let book_url = resolved.book_url.clone();
            let toc_url = resolved.toc_url.clone();
            async move {
                let resolved_for_chapter = crate::api::handlers::book::ResolvedBook {
                    book_url,
                    toc_url,
                    source,
                };
                let outcome = async {
                    let url = resolve_chapter_url(
                        &state,
                        &user_ns,
                        &book_id,
                        &resolved_for_chapter,
                        index,
                    )
                    .await?;
                    let data = crate::api::handlers::book::content::read_chapter_content(
                        &state,
                        &user_ns,
                        Some(&book_id),
                        Some(&url),
                        crate::api::handlers::book::content::ChapterReadOptions {
                            index: Some(index),
                            variant,
                            do_refresh,
                            known_version_id: None,
                        },
                    )
                    .await?;
                    Ok::<_, Problem>((index, data))
                }
                .await;
                let item = match outcome {
                    Ok((index, data)) => BatchContentItem {
                        index,
                        data: Some(map_to_v1(&book_id, index, data)),
                        error: None,
                    },
                    Err(problem) => BatchContentItem {
                        index,
                        data: None,
                        error: Some(BatchContentError {
                            code: problem.code(),
                            detail: problem.detail_text().map(str::to_string),
                        }),
                    },
                };
                (position, item)
            }
        }))
        .buffer_unordered(BATCH_CONTENT_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    results.sort_unstable_by_key(|(position, _)| *position);
    let items = results.into_iter().map(|(_, item)| item).collect();
    Ok(axum::Json(BatchContentResponse { items }))
}

/// 服务端并发上限：与 `/reader3` 批量正文同一口径。
const BATCH_CONTENT_CONCURRENCY: usize = 4;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchContentRequest {
    pub indices: Vec<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BatchContentQuery {
    pub variant: Option<String>,
    pub refresh: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchContentResponse {
    pub items: Vec<BatchContentItem>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchContentItem {
    pub index: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub data: Option<ChapterContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error: Option<BatchContentError>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchContentError {
    pub code: ErrorCode,
    pub detail: Option<String>,
}

/// ChapterContentData → v1 形状（单章与批量共用）。
fn map_to_v1(
    book_id: &str,
    index: i32,
    data: crate::api::handlers::book::content::ChapterContentData,
) -> ChapterContent {
    ChapterContent {
        book_id: book_id.to_string(),
        index,
        format: detect_content_format(&data.content).to_string(),
        body: data.content,
        version: data.version_id.map(|id| ChapterVersionInfo {
            id: cv_id(id),
            kind: data.version_kind.unwrap_or_else(|| "upstream".to_string()),
            based_on: data.base_version_id.map(cv_id),
        }),
        edited: data.edited,
        latest_upstream_version_id: data.latest_upstream_version_id.map(cv_id),
        diff_base_version_id: data.diff_base_version_id.map(cv_id),
        latest_user_version_id: data.latest_user_version_id.map(cv_id),
        diff: data.diff.map(|diff| diff.ops),
        diff_error: data.diff_error,
        upstream_changed_since_base: data.upstream_changed,
        selection_source: data.selection_source,
        stale: data.stale,
        fetched_at: data.last_success_at.map(crate::api::v1::me::rfc3339_secs),
        last_attempt_at: data.last_attempt_at.map(crate::api::v1::me::rfc3339_secs),
        upstream_changed_at: data
            .upstream_changed_at
            .map(crate::api::v1::me::rfc3339_secs),
        edited_at: data.edited_at.map(crate::api::v1::me::rfc3339_secs),
    }
}

#[cfg(test)]
mod tests {
    use super::detect_content_format;

    /// 与前端 chapterRender.test.ts 的四条边界用例同口径（D15：判定收口到
    /// 服务端一处，判错的代价全在服务端——前端按 format 直接渲染）。
    #[test]
    fn detects_html_and_text_boundary_cases() {
        // 前端既有边界用例
        assert_eq!(
            detect_content_format("<b>没有闭合"),
            "text",
            "无闭合标签不算 HTML"
        );
        assert_eq!(detect_content_format("a<b>c"), "text");
        assert_eq!(detect_content_format("1 < 2 > 0"), "text");
        assert_eq!(detect_content_format("<p>hello</p>"), "html");
        // 补充边界
        assert_eq!(
            detect_content_format("<br>"),
            "html",
            "自闭合块级标签单独出现"
        );
        assert_eq!(
            detect_content_format("<p>a</p>明文"),
            "html",
            "HTML 与明文混排按 HTML 渲染（消毒层负责安全）"
        );
        // 纯文本基本盘
        assert_eq!(detect_content_format("第一章 正文。"), "text");
        assert_eq!(detect_content_format(""), "text");
    }
}
