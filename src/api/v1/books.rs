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
#[serde(rename_all = "camelCase")]
pub struct ListChaptersQuery {
    /// 上一页返回的 nextCursor；空表示第一页
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    /// 同步重抓目录（等价 `/reader3` 的 refresh=1；只对第一页生效）
    pub refresh: Option<bool>,
    /// 第一页响应里的 `tocVersion`：续页回传。目录在翻页途中被后台刷新
    /// 整表替换时（index 可能重新编号）返回 409 `STALE_CURSOR`
    pub toc_version: Option<i64>,
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
    /// 登记表已过 TTL：本次先返回旧目录，后台正在刷新；上游失败时也会
    /// 保持 true 继续服务旧数据（不再把「目录打不开」当过期代价）
    pub stale: bool,
    /// 目录是否抓全：false 表示只登记了部分章节（后台补全中/被截断），
    /// 客户端可稍后重取，不要把当前 `total` 当全书章节数
    pub complete: bool,
    /// 目录快照令牌（`book_toc_state.fetched_at`）：续页回传用于发现
    /// 「翻页途中后台刷新整表替换」；本地书没有登记表，恒为 null
    pub toc_version: Option<i64>,
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
///
/// `refresh=true` 是用户显式刷新：上游失败直接 502，不回退旧登记表
/// （静默成功会掩盖书源已失效）；普通读才用旧目录兜底并标 `stale`。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/chapters",
    tag = "books",
    operation_id = "listChapters",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("cursor" = Option<String>, Query, description = "上一页的 nextCursor"),
        ("limit" = Option<u32>, Query, description = "页大小，默认 50，上限 200"),
        ("refresh" = Option<bool>, Query, description = "同步重抓目录（只对第一页生效）"),
        ("tocVersion" = Option<i64>, Query, description = "第一页的 tocVersion，续页回传用于检测目录快照已刷新"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "章节页（按 index 升序）", body = ChapterPage),
        (status = 304, description = "If-None-Match 命中，无 body"),
        (status = 400, description = "limit 超上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "目录已刷新（tocVersion 不匹配），客户端从第一页重拉", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "游标无法解码", body = Problem, content_type = "application/problem+json"),
        (status = 502, description = "上游失败；refresh=true 时不回退旧目录，如实报错", body = Problem, content_type = "application/problem+json"),
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
    let ChapterList {
        chapters,
        stale,
        complete,
        toc_version,
    } = load_chapters(
        &state,
        &user.ns,
        &book_id,
        &resolved,
        refresh,
        after_index.is_none(),
    )
    .await?;

    // 续页发现目录快照已被后台刷新替换：index 可能重新编号，继续按旧游标
    // 切片会在接缝处重复/漏章。让客户端从第一页重拉一次。
    if after_index.is_some() {
        if let Some(sent_version) = query.toc_version {
            if toc_version != Some(sent_version) {
                return Err(
                    Problem::new(ErrorCode::StaleCursor).detail("目录已刷新，请从第一页重新拉取")
                );
            }
        }
    }

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
        stale,
        complete,
        toc_version,
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

/// 一次目录读取的结果：章节列表 + 新鲜度/完整性/快照令牌（随响应原样下发）。
struct ChapterList {
    chapters: Vec<BookChapter>,
    /// 登记表已过 TTL：本次用旧目录兜底，后台正在刷新
    stale: bool,
    /// 目录是否抓全（false = 只登记了部分章节）
    complete: bool,
    /// 目录快照令牌（`book_toc_state.fetched_at`）；本地书为 None
    toc_version: Option<i64>,
}

async fn load_chapters(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    resolved: &crate::api::handlers::book::ResolvedBook,
    refresh: bool,
    first_page: bool,
) -> Result<ChapterList, Problem> {
    use crate::service::local_epub_book::is_local_epub_url;
    use crate::service::local_txt_book::is_local_txt_url;

    // 本地书：目录由文件确定性生成，直接经本地服务按 index 取章（不写登记表）
    if is_local_txt_url(&resolved.book_url) {
        let chapters = state
            .local_txt_book_service
            .get_chapter_list(user_ns, &resolved.book_url)
            .await
            .map_err(Problem::from)?;
        return Ok(ChapterList {
            chapters,
            stale: false,
            complete: true,
            toc_version: None,
        });
    }
    if is_local_epub_url(&resolved.book_url) {
        let chapters = state
            .local_epub_book_service
            .get_chapter_list(user_ns, &resolved.book_url)
            .await
            .map_err(Problem::from)?;
        return Ok(ChapterList {
            chapters,
            stale: false,
            complete: true,
            toc_version: None,
        });
    }

    // 网络书：登记表优先；**过期不阻塞**——先返回旧目录（stale=true）并拉起
    // 后台刷新（stale-while-revalidate），只有 refresh=true 才同步重抓。
    // 这样 TTL 过期不会把开书变成「串行抓全部分页」（慢源上开书要等上界未知
    // 的往返），上游挂掉也还能读旧目录，而不是抱着上千章登记行报错。
    // 分页续页只读裸登记行，避免翻页途中重抓把已返回页的 index 打乱。
    if !refresh {
        // 裸登记行读取（不看 TTL）：TTL 过期由下面的 stale 标记 + 后台刷新处理
        let cached = state
            .book_service
            .load_chapter_list_cache(user_ns, book_id)
            .await;
        if let Some(list) = cached.map_err(Problem::from)? {
            if !list.is_empty() {
                let toc_state = state
                    .book_service
                    .get_toc_state(book_id)
                    .await
                    .map_err(Problem::from)?;
                let (complete, fresh, fetched_at) = match toc_state {
                    Some(row) => (
                        row.complete,
                        crate::service::book_service::BookService::toc_state_is_fresh(
                            row.complete,
                            row.fetched_at,
                        ),
                        row.fetched_at,
                    ),
                    // 有登记行却没有状态行（移出书架/清缓存后）：按过期处理，
                    // 旧目录照常服务，后台重新抓取
                    None => (false, false, 0),
                };
                if !fresh && first_page {
                    spawn_toc_refresh(state, user_ns, book_id, resolved);
                }
                return Ok(ChapterList {
                    chapters: list,
                    stale: !fresh,
                    complete,
                    toc_version: Some(fetched_at),
                });
            }
        }
    }

    let toc_url = resolve_toc_url(state, user_ns, book_id, resolved).await?;
    let chapters = state
        .book_service
        .get_chapter_list_with_cache(user_ns, book_id, &resolved.source, &toc_url, refresh)
        .await
        .map_err(Problem::from)?;
    // 刚抓完的状态由 store_chapter_list 写入；截断/未完成会落 complete=false
    let toc_state = state
        .book_service
        .get_toc_state(book_id)
        .await
        .map_err(Problem::from)?;
    let complete = toc_state.map(|row| row.complete).unwrap_or(false);
    let toc_version = Some(toc_state.map(|row| row.fetched_at).unwrap_or(0));
    Ok(ChapterList {
        chapters,
        stale: false,
        complete,
        toc_version,
    })
}

/// 目录地址：登记表 > 书籍详情（详情缓存按 bookId，顺带回填 toc_url）> book_url。
async fn resolve_toc_url(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    resolved: &crate::api::handlers::book::ResolvedBook,
) -> Result<String, Problem> {
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
    Ok(crate::util::text::repair_encoded_url(&toc_url))
}

/// 过期登记表的后台刷新（single-flight）：与 `/reader3` 的后台补全共用
/// `try_begin_toc_fill` 守卫，同一本书同一时刻只跑一个。失败只记日志——
/// 请求已经拿到旧目录，下一次读取会在 TTL 仍过期时再次尝试。
fn spawn_toc_refresh(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
    resolved: &crate::api::handlers::book::ResolvedBook,
) {
    let Some(guard) = state.book_service.try_begin_toc_fill(user_ns, book_id) else {
        return;
    };
    let state = state.clone();
    let user_ns = user_ns.to_string();
    let book_id = book_id.to_string();
    let resolved = resolved.clone();
    tokio::spawn(async move {
        let _guard = guard;
        let toc_url = match resolve_toc_url(&state, &user_ns, &book_id, &resolved).await {
            Ok(toc_url) => toc_url,
            Err(error) => {
                tracing::warn!(?error, book_id, "目录后台刷新解析 tocUrl 失败");
                return;
            }
        };
        match state
            .book_service
            .refresh_chapter_list_if_current(
                &user_ns,
                &book_id,
                &resolved.book_url,
                &resolved.source,
                &toc_url,
            )
            .await
        {
            Ok(true) => {}
            // 抓取期间换源/改绑：旧源的整表结果丢弃，不能覆盖新源目录
            Ok(false) => tracing::warn!(book_id, "目录刷新期间书籍已换源，丢弃旧源结果"),
            Err(error) => {
                tracing::warn!(%error, book_id, "目录后台刷新失败，继续服务旧登记表")
            }
        }
    });
}
