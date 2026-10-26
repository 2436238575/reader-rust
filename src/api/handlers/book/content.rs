//! 章节正文端点：单章/批量读取（版本解析、强刷、超期后台回源）与正文缓存清理。

use super::resolve_book;
use crate::api::extract::QueryOrBody;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::service::local_epub_book::is_local_epub_url;
use crate::service::local_txt_book::is_local_txt_url;
use crate::util::text::repair_encoded_url;
use axum::{extract::State, Json};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct BookContentRequest {
    #[serde(rename = "chapterUrl")]
    pub chapter_url: Option<String>,
    #[serde(rename = "bookId")]
    pub book_id: Option<String>,
    pub index: Option<i32>,
    pub refresh: Option<i32>,
    /// `upstream` 返回上游原版（不读编辑版）；缺省/其它值返回编辑版优先。
    pub variant: Option<String>,
}

/// 批量正文（预加载）单次章节数上限。
pub const MAX_BATCH_CONTENT_CHAPTERS: usize = 10;
/// 批量正文的章节读取并发上限：预加载不该打满上游。
const BATCH_CONTENT_CONCURRENCY: usize = 4;

#[derive(Debug, Deserialize)]
pub struct BatchContentChapter {
    pub index: Option<i32>,
    #[serde(rename = "chapterUrl")]
    pub chapter_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BookContentsBatchRequest {
    /// 网络书必填；本地 TXT/EPUB（chapterUrl 自带 local: 前缀）可缺省。
    #[serde(rename = "bookId")]
    pub book_id: Option<String>,
    pub chapters: Vec<BatchContentChapter>,
    /// 与单章接口同义：`upstream` 返回上游原版，缺省/其它值返回默认版本。
    pub variant: Option<String>,
}

/// 批量接口里单章的结果：某章失败只写 `error`，不影响其他章节。
#[derive(Debug, Serialize)]
pub struct BatchContentItem {
    pub index: i32,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// 正文版本：默认版本（用户选择 > 用户版本 > 最新上游），或只预览最新上游。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentVariant {
    Default,
    Upstream,
}

impl ContentVariant {
    fn parse(value: Option<&str>) -> Self {
        if value.is_some_and(|v| v.trim().eq_ignore_ascii_case("upstream")) {
            Self::Upstream
        } else {
            Self::Default
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Upstream => "upstream",
        }
    }
}

/// 章节正文响应。`diff` 只表示「当前用户版本相对它的基准版本改了什么」；
/// 上游版本之间的差异走历史面板的版本 diff 接口。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterContentData {
    pub content: String,
    /// 当前返回的是否为用户版本（编辑版或手动导入）。
    pub edited: bool,
    /// `default` / `upstream`。
    pub variant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_version_id: Option<i64>,
    /// 本次 diff 使用的基准版本；始终是最新上游版本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_base_version_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_user_version_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_upstream_version_id: Option<i64>,
    /// 用户版本的基准上游之后，上游是否又变过。
    pub upstream_changed: bool,
    /// `selected` / `edited` / `upstream` / `upstream_preview`。
    pub selection_source: String,
    /// 当前返回的是旧数据：到期未回源，或最近一次回源失败。
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_changed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<ChapterDiffData>,
    /// diff 算不出来的原因（正文仍然可用）。前端在 diff 面板里展示。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChapterDiffData {
    pub ops: Vec<crate::service::chapter_diff_service::DiffOp>,
}

/// 网络书的正文响应：阅读页 diff 始终拿「当前用户版本 vs 最新上游版本」。
///
/// 评论源只对最新上游正文取段号，diff 标记必须和最新上游的段落对齐；
/// `base_version_id` 只记录编辑时基于哪一版，留作历史信息。
async fn network_content_data(
    state: &AppState,
    book_id: &str,
    chapter_url: &str,
    read: crate::service::book_service::ChapterContentRead,
    force_upstream: bool,
) -> ChapterContentData {
    let view = read.view;
    let is_user = view.kind != "upstream";
    let upstream_changed = is_user
        && match view.base_version_id {
            Some(base) => Some(base) != read.latest_upstream_version_id,
            None => read.latest_upstream_version_id.is_some(),
        };
    let diff_base_id = read.latest_upstream_version_id;
    let (diff, diff_error) = if is_user {
        match diff_base_id {
            Some(base_id) => {
                match state
                    .chapter_content_service
                    .version(book_id, chapter_url, base_id)
                    .await
                {
                    Ok(Some(base)) if base.content.trim().is_empty() => {
                        (None, Some("最新上游正文为空，未计算差异".to_string()))
                    }
                    Ok(Some(base)) => {
                        match state
                            .chapter_diff_service
                            .diff_texts(&base.content, &view.content)
                        {
                            Ok(ops) => (Some(ChapterDiffData { ops }), None),
                            Err(reason) => (None, Some(reason.to_string())),
                        }
                    }
                    Ok(None) => (None, Some("最新上游版本不可读，未计算差异".to_string())),
                    Err(error) => (None, Some(error.to_string())),
                }
            }
            None => (None, Some("最新上游版本不可读，未计算差异".to_string())),
        }
    } else {
        (None, None)
    };
    let selection_source = if force_upstream {
        "upstream_preview"
    } else if read.selected_version_id.is_some() {
        "selected"
    } else if read.latest_user_version_id == Some(view.version_id) {
        "edited"
    } else {
        "upstream"
    };
    ChapterContentData {
        content: view.content,
        edited: is_user,
        variant: if force_upstream {
            ContentVariant::Upstream.as_str()
        } else {
            ContentVariant::Default.as_str()
        }
        .to_string(),
        version_id: Some(view.version_id),
        version_kind: Some(view.kind),
        base_version_id: view.base_version_id,
        diff_base_version_id: if is_user { diff_base_id } else { None },
        latest_user_version_id: read.latest_user_version_id,
        latest_upstream_version_id: read.latest_upstream_version_id,
        upstream_changed,
        selection_source: selection_source.to_string(),
        stale: read.stale,
        last_attempt_at: read.last_attempt_at,
        last_success_at: read.last_success_at,
        upstream_changed_at: read.upstream_changed_at,
        edited_at: read.edited_at,
        diff,
        diff_error,
    }
}

/// 本地书的正文响应：上游原文来自本地文件，用户版本来自正文池。
/// 阅读页 diff 也始终以当前本地原文为基准（本地书没有版本化上游）。
async fn local_content_data(
    state: &AppState,
    upstream: Option<String>,
    user_view: Option<crate::service::chapter_content_service::ChapterVersionView>,
    latest_user_version_id: Option<i64>,
    force_upstream: bool,
) -> ChapterContentData {
    let has_user = user_view.is_some();
    let (content, version_id, version_kind, base_version_id, diff, diff_error) = match user_view {
        Some(view) if !force_upstream => {
            let (diff, diff_error) = match upstream.as_deref() {
                Some(base) => match state.chapter_diff_service.diff_texts(base, &view.content) {
                    Ok(ops) => (Some(ChapterDiffData { ops }), None),
                    Err(reason) => (None, Some(reason.to_string())),
                },
                None => (None, Some("本地原文不可读，未计算差异".to_string())),
            };
            (
                view.content,
                Some(view.version_id),
                Some(view.kind),
                view.base_version_id,
                diff,
                diff_error,
            )
        }
        _ => (upstream.unwrap_or_default(), None, None, None, None, None),
    };
    ChapterContentData {
        content,
        edited: has_user && !force_upstream,
        variant: if force_upstream {
            ContentVariant::Upstream.as_str()
        } else {
            ContentVariant::Default.as_str()
        }
        .to_string(),
        version_id,
        version_kind,
        base_version_id,
        diff_base_version_id: None,
        latest_user_version_id,
        latest_upstream_version_id: None,
        upstream_changed: false,
        selection_source: if force_upstream {
            "upstream_preview"
        } else if has_user {
            "edited"
        } else {
            "upstream"
        }
        .to_string(),
        stale: false,
        last_attempt_at: None,
        last_success_at: None,
        upstream_changed_at: None,
        edited_at: None,
        diff,
        diff_error,
    }
}

#[derive(Debug, Deserialize)]
pub struct DeleteCacheRequest {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
}

pub async fn get_book_content(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<BookContentRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let variant = ContentVariant::parse(req.variant.as_deref());
    let data = read_chapter_content(
        &state,
        &user_ns,
        req.book_id.as_deref(),
        req.chapter_url.as_deref(),
        req.index,
        variant,
        req.refresh.unwrap_or(0) > 0,
    )
    .await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(data).unwrap_or_default(),
    )))
}

/// 批量读取章节正文（预加载用）：一次最多 10 章、并发上限 4。
///
/// 每章独立成败——失败章节 `data` 为 null、原因写进 `error`，其余章节照常
/// 返回；整体始终 HTTP 200。章节数量或 chapterUrl 缺失这类请求本身的问题
/// 是 400（整批拒绝，避免半执行）。不做 refresh：预加载不需要强刷。
pub async fn get_book_contents(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<BookContentsBatchRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if req.chapters.is_empty() || req.chapters.len() > MAX_BATCH_CONTENT_CHAPTERS {
        return Err(AppError::BadRequest(format!(
            "chapters 需要提供 1 到 {MAX_BATCH_CONTENT_CHAPTERS} 条"
        )));
    }
    if let Some(position) = req.chapters.iter().position(|chapter| {
        chapter
            .chapter_url
            .as_deref()
            .is_none_or(|v| v.trim().is_empty())
    }) {
        return Err(AppError::BadRequest(format!(
            "第 {} 条章节缺少 chapterUrl",
            position + 1
        )));
    }

    let user_ns = user.0.ns.clone();
    let variant = ContentVariant::parse(req.variant.as_deref());
    let book_id = req.book_id.filter(|v| !v.trim().is_empty());

    let mut results: Vec<(usize, BatchContentItem)> = futures::stream::iter(
        req.chapters
            .into_iter()
            .enumerate()
            .map(|(position, chapter)| {
                let state = state.clone();
                let user_ns = user_ns.clone();
                let book_id = book_id.clone();
                async move {
                    let index = chapter.index.unwrap_or(0);
                    let outcome = read_chapter_content(
                        &state,
                        &user_ns,
                        book_id.as_deref(),
                        chapter.chapter_url.as_deref(),
                        chapter.index,
                        variant,
                        false,
                    )
                    .await;
                    let item = match outcome {
                        Ok(data) => BatchContentItem {
                            index,
                            data: Some(serde_json::to_value(data).unwrap_or_default()),
                            error: None,
                        },
                        Err(error) => BatchContentItem {
                            index,
                            data: None,
                            error: Some(error.client_message()),
                        },
                    };
                    (position, item)
                }
            }),
    )
    .buffer_unordered(BATCH_CONTENT_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    // buffer_unordered 按完成顺序产出：按请求顺序回排，前端按下标对号入座。
    results.sort_unstable_by_key(|(position, _)| *position);
    let chapters: Vec<BatchContentItem> = results.into_iter().map(|(_, item)| item).collect();

    Ok(Json(ApiResponse::ok(
        serde_json::to_value(chapters).unwrap_or_default(),
    )))
}
/// 单章正文的完整读取链路：本地 TXT/EPUB 分支 + 网络书分支
/// （版本解析、强刷、超期后台回源）。
///
/// 单章 getBookContent 与批量 getBookContents 共用这一份实现，
/// 避免两套版本解析/回源/本地书逻辑各自漂移。
async fn read_chapter_content(
    state: &AppState,
    user_ns: &str,
    book_id: Option<&str>,
    chapter_url: Option<&str>,
    index: Option<i32>,
    variant: ContentVariant,
    do_refresh: bool,
) -> Result<ChapterContentData, AppError> {
    let force_upstream = variant == ContentVariant::Upstream;

    if chapter_url.is_some_and(is_local_txt_url) {
        let url =
            chapter_url.ok_or_else(|| AppError::BadRequest("chapterUrl required".to_string()))?;
        let chapter_url = if is_local_txt_url(url) && !url.contains('#') {
            let index = index.unwrap_or(0).max(0) as usize;
            format!(
                "{}#{}",
                repair_encoded_url(url).trim_end_matches('#'),
                index
            )
        } else {
            repair_encoded_url(url)
        };
        return local_book_content(
            state,
            user_ns,
            book_id.filter(|v| !v.trim().is_empty()),
            &chapter_url,
            variant,
            |ns, url| {
                let service = state.local_txt_book_service.clone();
                let ns = ns.to_string();
                let url = url.to_string();
                async move { service.get_content(&ns, &url).await }
            },
            "本地 TXT",
        )
        .await;
    }

    if chapter_url.is_some_and(is_local_epub_url) {
        let url =
            chapter_url.ok_or_else(|| AppError::BadRequest("chapterUrl required".to_string()))?;
        let chapter_url = if is_local_epub_url(url) && !url.contains('#') {
            let index = index.unwrap_or(0).max(0) as usize;
            format!(
                "{}#{}",
                repair_encoded_url(url).trim_end_matches('#'),
                index
            )
        } else {
            repair_encoded_url(url)
        };
        return local_book_content(
            state,
            user_ns,
            book_id.filter(|v| !v.trim().is_empty()),
            &chapter_url,
            variant,
            |ns, url| {
                let service = state.local_epub_book_service.clone();
                let ns = ns.to_string();
                let url = url.to_string();
                async move { service.get_content(&ns, &url).await }
            },
            "本地 EPUB",
        )
        .await;
    }

    let book_id = book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let chapter_url = chapter_url
        .filter(|v| !v.trim().is_empty())
        .map(repair_encoded_url)
        .ok_or_else(|| AppError::BadRequest("chapterUrl required".to_string()))?;
    let resolved = resolve_book(state, user_ns, book_id).await?;

    // 强刷：抓一份新的上游版本。失败时 refresh_chapter_content 已经回退到
    // 已有缓存；没有缓存才把错误抛出来。
    if do_refresh {
        state
            .book_service
            .refresh_chapter_content(user_ns, book_id, &resolved.source, &chapter_url)
            .await?;
    }

    let read = state
        .book_service
        .read_content(
            user_ns,
            book_id,
            &resolved.source,
            &chapter_url,
            force_upstream,
        )
        .await?;
    // 超过 24 小时没回源：当前请求先返回缓存，后台再抓一次。
    if read.needs_revalidate && !do_refresh {
        let service = state.book_service.clone();
        let source = resolved.source.clone();
        let user_ns = user_ns.to_string();
        let book_id = book_id.to_string();
        let chapter_url = chapter_url.clone();
        tokio::spawn(async move {
            service
                .revalidate_chapter(&user_ns, &book_id, &source, &chapter_url)
                .await;
        });
    }
    Ok(network_content_data(state, book_id, &chapter_url, read, force_upstream).await)
}

/// 本地书的正文读取。用户版本来自正文池，上游原文来自本地文件。
async fn local_book_content<F, Fut>(
    state: &AppState,
    user_ns: &str,
    book_id: Option<&str>,
    chapter_url: &str,
    variant: ContentVariant,
    read_upstream: F,
    label: &str,
) -> Result<ChapterContentData, AppError>
where
    F: FnOnce(&str, &str) -> Fut,
    Fut: std::future::Future<Output = Result<String, AppError>>,
{
    let force_upstream = variant == ContentVariant::Upstream;
    let user_view = match book_id {
        Some(book_id) if !force_upstream => {
            state
                .chapter_content_service
                .resolve(user_ns, book_id, chapter_url, false)
                .await?
        }
        _ => None,
    };
    let latest_user_version_id = match book_id {
        Some(book_id) => state
            .chapter_content_service
            .latest(book_id, chapter_url)
            .await?
            .and_then(|row| row.latest_user_version_id),
        None => None,
    };
    let upstream = match read_upstream(user_ns, chapter_url).await {
        Ok(content) => Some(content),
        Err(error) if !force_upstream && user_view.is_some() => {
            tracing::warn!(%error, "{}原文读取失败，只返回用户版本", label);
            None
        }
        Err(error) => return Err(error),
    };
    let data = local_content_data(
        state,
        upstream,
        user_view,
        latest_user_version_id,
        force_upstream,
    )
    .await;
    Ok(data)
}

pub async fn delete_book_cache(
    State(state): State<AppState>,
    user: CurrentUser,
    QueryOrBody(req): QueryOrBody<DeleteCacheRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    // bookId 会拼进 toc/详情/候选清单的缓存文件路径：格式 + 存在 + 归属校验
    //（与书籍级接口一致）；`../../x` 这类输入在这里就 404，到不了文件系统
    resolve_book(&state, &user_ns, &book_id).await?;

    // 章节列表 + 换源候选；正文历史不删，评论与书籍详情也不动
    let deleted_chapter_list = state
        .book_service
        .chapter_list_cache_exists(&user_ns, &book_id)
        .await;
    if deleted_chapter_list {
        state
            .book_service
            .delete_chapter_list_cache(&user_ns, &book_id)
            .await?;
    }
    let _ = state
        .book_service
        .delete_book_sources_cache(&user_ns, &book_id)
        .await;

    Ok(Json(ApiResponse::ok(serde_json::json!({
        "deleted": true,
        "contentCache": false,
        "chapterListCache": deleted_chapter_list
    }))))
}
