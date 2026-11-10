//! `GET /v1/books/{bookId}/chapters`：章节列表（对外身份是 index）。
//!
//! 章节身份按 P-1 的登记表落地：**对外只有 `(bookId, index)`**，URL 不出现
//! 在请求参数里（红线 6）。数据来源三分支：
//! - 网络书：登记表优先（`chapters` 表），空则走目录抓取路径填充登记表——
//!   与 `/reader3/getChapterList` 共用同一份服务层，缓存/熔断/后台补全语义一致；
//! - 本地书（TXT/EPUB）：不写登记表（目录由文件确定性生成），直接经本地
//!   服务按 index 取章（D 注记）。
//!
//! 标题覆盖层（用户改名）在响应出口叠加，`titleOverridden` 显式标注；
//! ETag 基于最终序列化字节，覆盖变化自然改变 ETag。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::V1User;
use crate::api::AppState;
use crate::model::book_chapter::BookChapter;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

use crate::api::etag::if_none_match_matches;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/books/{bookId}/chapters", get(list_chapters))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ListChaptersQuery {
    /// 上一页返回的 nextCursor；空表示第一页
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    /// 同步重抓目录（等价 `/reader3` 的 refresh=1；只对第一页生效）
    pub refresh: Option<bool>,
}

/// 章节条目：身份是 `index`（书籍内稳定序号，换源整表替换）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterItem {
    pub index: i32,
    pub title: String,
    /// 只读诊断字段（红线 6 例外：仅出现在响应里）。过渡期阅读器仍靠它
    /// 调 /reader3 的评论/配图/编辑接口与 IndexedDB 缓存键；这些域迁走后删除
    pub url: String,
    /// 用户改名覆盖生效中（改名走 `chapter_title_overrides`，不写回上游标题）
    pub title_overridden: bool,
    pub is_vip: bool,
    pub is_pay: bool,
    pub is_volume: bool,
    pub volume: Option<String>,
    pub word_count: Option<String>,
}

/// 游标分页容器（契约 §5.4）：`nextCursor: null` 表示到底。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChapterPage {
    pub items: Vec<ChapterItem>,
    pub next_cursor: Option<String>,
    pub total: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct CursorPayload {
    index: i32,
}

fn encode_cursor(index: i32) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&CursorPayload { index }).expect("cursor json"))
}

fn decode_cursor(raw: &str) -> Result<i32, Problem> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| Problem::new(ErrorCode::InvalidCursor).detail("游标无法解码"))?;
    let payload: CursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| Problem::new(ErrorCode::InvalidCursor).detail("游标无法解码"))?;
    Ok(payload.index)
}

/// 章节列表。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters",
    tag = "books",
    operation_id = "listChapters",
    params(
        ("bookId" = String, Path, description = "书籍 id（bk_<n>）"),
        ("cursor" = Option<String>, Query, description = "上一页的 nextCursor"),
        ("limit" = Option<u32>, Query, description = "页大小，默认 50，上限 200"),
        ("refresh" = Option<bool>, Query, description = "同步重抓目录（只对第一页生效）"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "章节页（按 index 升序）", body = ChapterPage),
        (status = 304, description = "If-None-Match 命中，无 body"),
        (status = 400, description = "limit 超上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "游标无法解码", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_chapters(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    Query(query): Query<ListChaptersQuery>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(Problem::new(ErrorCode::MalformedRequest)
            .detail(format!("limit 必须在 1..={MAX_LIMIT} 之间")));
    }
    let after_index = match query.cursor.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(decode_cursor(raw)?),
        _ => None,
    };

    // 书籍不存在用精确码（通用 NOT_FOUND 兜底只服务未预期路径）
    let resolved = crate::api::handlers::book::resolve_book(&state, &user.ns, &book_id)
        .await
        .map_err(|error| match error {
            crate::error::error::AppError::NotFound(_) => {
                Problem::new(ErrorCode::BookNotFound).detail("书籍不存在")
            }
            other => Problem::from(other),
        })?;

    // refresh 与 /reader3 的 refresh=1 同语义：不读缓存，同步重抓后整表替换。
    // 只对第一页生效——分页中途重抓会让后续页与已返回页错位；失败时旧登记行
    // 保留（不做先删后抓），下一次普通读还能用。
    let refresh = query.refresh.unwrap_or(false) && after_index.is_none();
    let chapters = load_chapters(
        &state,
        &user.ns,
        &book_id,
        &resolved,
        refresh,
        after_index.is_none(),
    )
    .await?;

    // 标题覆盖层：读取时叠加，titleOverridden 显式标注
    let override_map = match crate::storage::db::repo::parse_book_id(&book_id) {
        Some(id) => state
            .chapter_title_override_repo
            .get_map(id)
            .await
            .unwrap_or_default(),
        None => Default::default(),
    };

    let mut items: Vec<ChapterItem> = Vec::with_capacity(chapters.len());
    for chapter in &chapters {
        let overridden = override_map.get(&chapter.url).cloned();
        items.push(ChapterItem {
            index: chapter.index,
            title: overridden.clone().unwrap_or_else(|| chapter.title.clone()),
            url: chapter.url.clone(),
            title_overridden: overridden.is_some(),
            is_vip: chapter.is_vip,
            is_pay: chapter.is_pay,
            is_volume: chapter.is_volume,
            volume: chapter.volume.clone(),
            word_count: chapter.word_count.clone(),
        });
    }
    let total = items.len() as u32;
    if let Some(after) = after_index {
        items.retain(|item| item.index > after);
    }
    let has_more = items.len() as u32 > limit;
    items.truncate(limit as usize);
    let next_cursor = has_more
        .then(|| items.last().map(|last| encode_cursor(last.index)))
        .flatten();

    let page = ChapterPage {
        items,
        next_cursor,
        total: Some(total),
    };
    let body = serde_json::to_vec(&page).expect("chapter page serializes");
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

async fn load_chapters(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    resolved: &crate::api::handlers::book::ResolvedBook,
    refresh: bool,
    first_page: bool,
) -> Result<Vec<BookChapter>, Problem> {
    use crate::service::local_epub_book::is_local_epub_url;
    use crate::service::local_txt_book::is_local_txt_url;

    // 本地书：目录由文件确定性生成，直接经本地服务按 index 取章（不写登记表）
    if is_local_txt_url(&resolved.book_url) {
        return state
            .local_txt_book_service
            .get_chapter_list(user_ns, &resolved.book_url)
            .await
            .map_err(Problem::from);
    }
    if is_local_epub_url(&resolved.book_url) {
        return state
            .local_epub_book_service
            .get_chapter_list(user_ns, &resolved.book_url)
            .await
            .map_err(Problem::from);
    }

    // 网络书：登记表优先（P-1 的持久载体）；空则走抓取路径填充登记表——
    // 与 /reader3/getChapterList 共用同一份服务层。
    // 第一页按 TTL 判定新鲜度（完整 1h / 未完成 60s），过期即重抓；分页续页
    // 只读裸登记行，避免 TTL 在翻页途中触发重抓、把已返回页的 index 打乱。
    if !refresh {
        let existing = if first_page {
            state
                .book_service
                .load_fresh_chapter_list_cache(user_ns, book_id)
                .await
        } else {
            state
                .book_service
                .load_chapter_list_cache(user_ns, book_id)
                .await
        };
        if let Some(list) = existing.map_err(Problem::from)? {
            if !list.is_empty() {
                return Ok(list);
            }
        }
    }
    let toc_url = match resolved
        .toc_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(toc_url) => toc_url.to_string(),
        None => state
            .book_service
            .get_book_info(
                user_ns,
                book_id,
                &resolved.source,
                &resolved.book_url,
                false,
            )
            .await
            .map_err(Problem::from)?
            .toc_url
            .unwrap_or_else(|| resolved.book_url.clone()),
    };
    let toc_url = crate::util::text::repair_encoded_url(&toc_url);
    state
        .book_service
        .get_chapter_list_with_cache(user_ns, book_id, &resolved.source, &toc_url, refresh)
        .await
        .map_err(Problem::from)
}
