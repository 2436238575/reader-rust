//! `GET /v1/sources`：书源列表（游标分页 + ETag + `q` 过滤）。
//!
//! 本批只迁读链路：列表页用摘要（规则 JSON 可达 MB 级/源，不下发）；
//! 单源详情与写链路（save/test/delete/import）随 sources 写批次一起迁移，
//! 编辑器在那之前继续走 `/reader3`。两套读路径读同一张表，状态一致。
//!
//! 排序由服务端固定为 `id` 升序（不可变、keyset 稳定）：`updated_at` 会随
//! 编辑变化，做游标会漏行/重行；展示排序各端自行处理。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::V1User;
use crate::api::AppState;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 列表默认页大小与上限（契约 §5.4：超限 400 而不是静默截断）。
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/sources", get(list_sources))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ListSourcesQuery {
    /// 名称或 URL 的大小写不敏感子串过滤
    pub q: Option<String>,
    /// 上一页返回的 nextCursor；空表示第一页
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// 书源摘要：展示字段 + `exploreUrl`，不含规则 JSON。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceSummary {
    /// 不透明 id（`src_<n>`），客户端不解析
    pub id: String,
    pub name: String,
    pub url: String,
    pub group: Option<String>,
    #[serde(rename = "type")]
    pub source_type: Option<i32>,
    pub enabled: bool,
    pub enabled_explore: bool,
    pub enabled_cookie_jar: bool,
    /// 源材料里的更新时间（服务端存毫秒），输出 RFC 3339 UTC
    #[schema(format = DateTime)]
    pub updated_at: Option<String>,
    pub respond_time: Option<i64>,
    pub custom_order: Option<i32>,
    pub weight: Option<i32>,
    pub explore_url: Option<String>,
}

/// 游标分页容器（契约 §5.4）：`nextCursor: null` 表示到底。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourcePage {
    pub items: Vec<SourceSummary>,
    pub next_cursor: Option<String>,
    /// 过滤后的总数（小表直接算，不做惰性统计）
    pub total: Option<u32>,
}

/// 游标载荷：上一页最后一条的 id。语义上是 keyset（不可变主键之后的下一页），
/// 当前实现为内存切片，不下沉到 SQL。
#[derive(Serialize, Deserialize)]
struct CursorPayload {
    id: i64,
}

fn encode_cursor(id: i64) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&CursorPayload { id }).expect("cursor json"))
}

fn decode_cursor(raw: &str) -> Result<i64, Problem> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| Problem::new(ErrorCode::InvalidCursor).detail("游标无法解码"))?;
    let payload: CursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| Problem::new(ErrorCode::InvalidCursor).detail("游标无法解码"))?;
    Ok(payload.id)
}

/// 列表响应的强 ETag：对实际返回的 JSON 取 md5，同一份数据字节级稳定。
/// （契约 §10 标注列表用弱 ETag；这里对精确字节做哈希，强验证器更严格，
/// 语义上成立且与章节目录沿用同一做法。）
fn response_etag(bytes: &[u8]) -> HeaderValue {
    let digest = Md5::digest(bytes);
    HeaderValue::from_str(&format!("\"{}\"", hex::encode(digest)))
        .expect("md5 hex + 引号必为合法 header 值")
}

/// `If-None-Match` 的宽松比较（RFC 9110 §13.1.2），与章节目录同一实现。
fn if_none_match_matches(if_none_match: Option<&HeaderValue>, etag: &HeaderValue) -> bool {
    let Some(header_value) = if_none_match else {
        return false;
    };
    let Ok(raw) = header_value.to_str() else {
        return false;
    };
    let raw = raw.trim();
    if raw == "*" {
        return true;
    }
    let Some(current) = etag.to_str().ok() else {
        return false;
    };
    raw.split(',').any(|candidate| {
        candidate
            .trim()
            .strip_prefix("W/")
            .unwrap_or(candidate.trim())
            == current
    })
}

/// 书源列表。
///
/// **排序固定为 `id` 升序（创建顺序）**：游标 keyset 依赖不可变排序键；
/// 旧 `/reader3/getBookSourceList` 是 `updated_at` 降序（最近编辑在前），
/// 切换后列表顺序语义随之变化（决策记录有档）。展示层如需其他顺序自行排序。
#[utoipa::path(
    get,
    path = "/v1/sources",
    tag = "sources",
    operation_id = "listSources",
    description = "书源摘要页。排序固定为 id 升序（创建顺序）；游标 keyset 依赖不可变排序键。",
    params(
        ("q" = Option<String>, Query, description = "名称或 URL 的大小写不敏感子串"),
        ("cursor" = Option<String>, Query, description = "上一页的 nextCursor"),
        ("limit" = Option<u32>, Query, description = "页大小，默认 50，上限 200"),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "书源摘要页", body = SourcePage),
        (status = 304, description = "If-None-Match 命中，无 body"),
        (status = 400, description = "limit 超上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "游标无法解码", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_sources(
    State(state): State<AppState>,
    V1User(user): V1User,
    Query(query): Query<ListSourcesQuery>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(Problem::new(ErrorCode::MalformedRequest)
            .detail(format!("limit 必须在 1..={MAX_LIMIT} 之间")));
    }
    let after_id = match query.cursor.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(decode_cursor(raw)?),
        _ => None,
    };
    let needle = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);

    let mut items: Vec<SourceSummary> = state
        .book_source_service
        .list_summary(&user.ns)
        .await
        .map_err(Problem::from)?
        .into_iter()
        .filter_map(|summary| {
            let id = summary.book_source_id?;
            if let Some(needle) = &needle {
                let name = summary.book_source_name.to_lowercase();
                let url = summary.book_source_url.to_lowercase();
                if !name.contains(needle.as_str()) && !url.contains(needle.as_str()) {
                    return None;
                }
            }
            Some(SourceSummary {
                id: format!("src_{id}"),
                name: summary.book_source_name,
                url: summary.book_source_url,
                group: summary.book_source_group,
                source_type: summary.book_source_type,
                // Legado 语义：未显式关闭即启用
                enabled: summary.enabled.unwrap_or(true),
                enabled_explore: summary.enabled_explore.unwrap_or(true),
                enabled_cookie_jar: summary.enabled_cookie_jar.unwrap_or(true),
                updated_at: summary
                    .last_update_time
                    .map(crate::api::v1::me::rfc3339_from_millis),
                respond_time: summary.respond_time,
                custom_order: summary.custom_order,
                weight: summary.weight,
                explore_url: summary.explore_url,
            })
        })
        .collect();
    // 当前实现：全量载入 + 内存切片（书源量级几百，够用；SQL keyset 下沉
    // 留待书源量增长）。排序键是 id 升序（不可变，翻页稳定）。
    items.sort_by_key(|item| {
        item.id
            .strip_prefix("src_")
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(i64::MAX)
    });
    let total = items.len() as u32;
    if let Some(after) = after_id {
        items.retain(|item| {
            item.id
                .strip_prefix("src_")
                .and_then(|raw| raw.parse::<i64>().ok())
                .is_some_and(|id| id > after)
        });
    }
    let has_more = items.len() as u32 > limit;
    items.truncate(limit as usize);
    let next_cursor = has_more
        .then(|| {
            items
                .last()
                .and_then(|last| last.id.strip_prefix("src_"))
                .and_then(|raw| raw.parse::<i64>().ok())
                .map(encode_cursor)
        })
        .flatten();

    let page = SourcePage {
        items,
        next_cursor,
        total: Some(total),
    };
    let body = serde_json::to_vec(&page).expect("source page serializes");
    let etag = response_etag(&body);
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
