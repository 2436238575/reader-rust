//! `/v1/sources`：书源列表（读）+ 写链路同步子集（详情/单条 upsert/批量
//! upsert/删除/批量删除/清空/删失效）。
//!
//! 列表用摘要（规则 JSON 可达 MB 级/源，不下发）；`GET /v1/sources/{id}`
//! 下发完整源材料（原样透传，含数字 `bookSourceId` 字段——身份参数仍走
//! `src_<n>`，与 D15/只读例外同思路）。
//!
//! 请求体是**源材料**：走 `book_source_from_value` 的宽松迁移（legado 旧
//! 字段名兼容），不做 `deny_unknown_fields`——导入材料带附加字段是常态。
//! `:import`/`:test`/`:login` 按契约是 202 Task，任务子系统（P2）落地前
//! 继续 `/reader3`；`PATCH /v1/sources/{id}` 随启停域一起迁。
//!
//! 排序由服务端固定为 `id` 升序（不可变、keyset 稳定）：`updated_at` 会随
//! 编辑变化，做游标会漏行/重行；展示排序各端自行处理。

use crate::api::etag::if_none_match_matches;
use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use crate::model::book_source::{book_source_from_value, BookSource};
use crate::service::book_source_service::{book_source_has_group, INVALID_BOOK_SOURCE_GROUP};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// 列表默认页大小与上限（契约 §5.4：超限 400 而不是静默截断）。
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

/// 单次批量 upsert / 批量删除的条数上限（导入材料常是整站打包，1000 覆盖；
/// 请求体上限单独放宽到 /reader3 的 64MB，见 `SAVE_SOURCES_BODY_LIMIT_BYTES`）。
const MAX_SOURCES_BATCH: usize = 1000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/sources",
            get(list_sources)
                .post(upsert_source)
                .delete(delete_all_sources),
        )
        .route("/v1/sources/{id}", get(get_source).delete(delete_source))
        .route("/v1/sources:batchDelete", post(batch_delete_sources))
        .route("/v1/sources:deleteInvalid", post(delete_invalid_sources))
        // 批量导入的请求体放宽：整站打包的源文件远超 /v1 默认 1MB。
        // 挂在 MethodRouter 上（内层），晚于 v1 mod 的外层 1MB 生效
        .route(
            "/v1/sources:batchUpsert",
            post(batch_upsert_sources).layer(axum::extract::DefaultBodyLimit::max(
                crate::api::router::SAVE_SOURCES_BODY_LIMIT_BYTES,
            )),
        )
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

// ─── 写链路（同步子集） ────────────────────────────────────────────────

/// 路径里的书源 id：`src_<n>`，解析不了按不存在处理。
fn parse_source_path_id(raw: &str) -> Result<i64, Problem> {
    raw.strip_prefix("src_")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| Problem::new(ErrorCode::SourceNotFound).detail("书源不存在"))
}

/// 源材料解析：走模型的宽松迁移（legado 旧字段名 / 字符串枚举兼容），
/// 未知字段原样忽略——导入材料带附加字段是常态，不做 strict。
fn source_from_raw(value: serde_json::Value) -> Result<BookSource, Problem> {
    let source = book_source_from_value(value).map_err(|error| {
        Problem::new(ErrorCode::MalformedRequest).detail(format!("书源 JSON 无法解析：{error}"))
    })?;
    if source.book_source_url.trim().is_empty() {
        return Err(Problem::new(ErrorCode::MalformedRequest)
            .detail("bookSourceUrl 不能为空（它是导入去重与保号的键）"));
    }
    Ok(source)
}

/// 完整书源（源材料透传）：编辑器按整条对象读写。身份参数是 `src_<n>`，
/// 材料里的 `bookSourceId`（数字）是源材料字段，原样保留。
#[utoipa::path(
    get,
    path = "/v1/sources/{id}",
    tag = "sources",
    operation_id = "getSource",
    params(("id" = String, Path, description = "书源 id（src_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "完整书源", body = BookSource),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书源不存在", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_source(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(id): Path<String>,
) -> Result<Json<BookSource>, Problem> {
    let numeric = parse_source_path_id(&id)?;
    let source = state
        .book_source_service
        .get_by_id(&user.ns, numeric)
        .await
        .map_err(Problem::from)?
        .ok_or_else(|| Problem::new(ErrorCode::SourceNotFound).detail("书源不存在"))?;
    Ok(Json(source))
}

/// 单条写入结果：线缆身份 `src_<n>`。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceWriteResult {
    pub id: String,
}

/// 单条全量 upsert：优先按自带 `bookSourceId` 更新，否则按 URL 保号归并。
/// 新建 201 / 更新 200。
#[utoipa::path(
    post,
    path = "/v1/sources",
    tag = "sources",
    operation_id = "upsertSource",
    request_body = BookSource,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 201, description = "已新建", body = SourceWriteResult),
        (status = 200, description = "已有同 URL / 同 id 书源，已替换", body = SourceWriteResult),
        (status = 400, description = "源材料无法解析或缺少 bookSourceUrl", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn upsert_source(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(value): V1Json<serde_json::Value>,
) -> Result<(StatusCode, Json<SourceWriteResult>), Problem> {
    let source = source_from_raw(value)?;
    let url = source.book_source_url.clone();
    // 新建/替换的判定：URL 命中（保号归并）**或**自带 id 命中（编辑改 URL 走
    // update 分支）都算替换——只看 URL 会把「编辑已有书源改 URL」误报成 201
    let existed = state
        .book_source_service
        .list_summary(&user.ns)
        .await
        .map_err(Problem::from)?
        .iter()
        .any(|summary| {
            summary.book_source_url == url
                || source
                    .book_source_id
                    .is_some_and(|id| summary.book_source_id == Some(id))
        });
    let id = state
        .book_source_service
        .save(&user.ns, source)
        .await
        .map_err(Problem::from)?;
    let status = if existed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(SourceWriteResult {
            id: format!("src_{id}"),
        }),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchUpsertSourcesRequest {
    /// 源材料数组（宽松迁移；单项解析失败整批 400）
    pub sources: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpsertSourcesResult {
    pub count: u32,
}

/// 批量导入/订阅/恢复：URL 去重优先（保本地 id），单事务提交。
#[utoipa::path(
    post,
    path = "/v1/sources:batchUpsert",
    tag = "sources",
    operation_id = "batchUpsertSources",
    request_body = BatchUpsertSourcesRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "写入条数", body = BatchUpsertSourcesResult),
        (status = 400, description = "sources 为空/超上限或单项无法解析", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "请求体超过批量导入上限", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn batch_upsert_sources(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<BatchUpsertSourcesRequest>,
) -> Result<Json<BatchUpsertSourcesResult>, Problem> {
    if request.sources.is_empty() || request.sources.len() > MAX_SOURCES_BATCH {
        return Err(Problem::new(ErrorCode::MalformedRequest)
            .detail(format!("sources 需要提供 1 到 {MAX_SOURCES_BATCH} 条")));
    }
    let mut sources = Vec::with_capacity(request.sources.len());
    for (index, value) in request.sources.into_iter().enumerate() {
        let parsed = match source_from_raw(value) {
            Ok(source) => source,
            Err(error) => {
                let detail = format!(
                    "第 {} 条源材料非法：{}",
                    index + 1,
                    error.detail_text().unwrap_or_default()
                );
                return Err(error.detail(detail));
            }
        };
        sources.push(parsed);
    }
    let count = sources.len() as u32;
    state
        .book_source_service
        .save_many(&user.ns, sources)
        .await
        .map_err(Problem::from)?;
    Ok(Json(BatchUpsertSourcesResult { count }))
}

/// 单条删除（幂等）：书架里绑定该源的书籍会失去书源（与 /reader3 同语义）。
#[utoipa::path(
    delete,
    path = "/v1/sources/{id}",
    tag = "sources",
    operation_id = "deleteSource",
    params(("id" = String, Path, description = "书源 id（src_<n>）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已删除（幂等）"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_source(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(id): Path<String>,
) -> Result<StatusCode, Problem> {
    let numeric = parse_source_path_id(&id)?;
    state
        .book_source_service
        .delete_by_id(&user.ns, numeric)
        .await
        .map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchDeleteSourcesRequest {
    pub ids: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteSourcesResult {
    pub deleted: u32,
}

/// 批量删除（幂等）：不存在的 id 忽略、不计数。
#[utoipa::path(
    post,
    path = "/v1/sources:batchDelete",
    tag = "sources",
    operation_id = "batchDeleteSources",
    request_body = BatchDeleteSourcesRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "删除条数", body = BatchDeleteSourcesResult),
        (status = 400, description = "ids 为空或超上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn batch_delete_sources(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<BatchDeleteSourcesRequest>,
) -> Result<Json<BatchDeleteSourcesResult>, Problem> {
    if request.ids.is_empty() || request.ids.len() > MAX_SOURCES_BATCH {
        return Err(Problem::new(ErrorCode::MalformedRequest)
            .detail(format!("ids 需要提供 1 到 {MAX_SOURCES_BATCH} 个")));
    }
    let mut deleted = 0u32;
    for id in &request.ids {
        // 非法 id 视同不存在，不送进 SQL
        let Ok(numeric) = parse_source_path_id(id) else {
            continue;
        };
        let removed = state
            .book_source_service
            .delete_by_id(&user.ns, numeric)
            .await
            .map_err(Problem::from)?;
        if removed {
            deleted += 1;
        }
    }
    Ok(Json(BatchDeleteSourcesResult { deleted }))
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAllSourcesResult {
    pub deleted: u64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteAllSourcesRequest {
    /// 必须逐字为 `DELETE_ALL`：清空全部书源是毁灭性操作，用确认体当门槛
    pub confirm: String,
}

/// 清空全部书源（同步；契约的 202 Task 化随任务子系统）。绑定该部署书源的
/// 书籍会失去书源，正文历史与登记行不受影响。
#[utoipa::path(
    delete,
    path = "/v1/sources",
    tag = "sources",
    operation_id = "deleteAllSources",
    request_body = DeleteAllSourcesRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "删除行数", body = DeleteAllSourcesResult),
        (status = 400, description = "confirm 不是 DELETE_ALL", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_all_sources(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<DeleteAllSourcesRequest>,
) -> Result<Json<DeleteAllSourcesResult>, Problem> {
    if request.confirm != "DELETE_ALL" {
        return Err(Problem::new(ErrorCode::MalformedRequest).detail("confirm 必须为 DELETE_ALL"));
    }
    let deleted = state
        .book_source_service
        .delete_all(&user.ns)
        .await
        .map_err(Problem::from)?;
    Ok(Json(DeleteAllSourcesResult { deleted }))
}

/// 删除失效书源：分组含「失效」标记的整行删除（与 `/reader3` 同口径）。
#[utoipa::path(
    post,
    path = "/v1/sources:deleteInvalid",
    tag = "sources",
    operation_id = "deleteInvalidSources",
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "删除条数", body = BatchDeleteSourcesResult),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_invalid_sources(
    State(state): State<AppState>,
    V1User(user): V1User,
) -> Result<Json<BatchDeleteSourcesResult>, Problem> {
    let invalid_ids = state
        .book_source_service
        .list_summary(&user.ns)
        .await
        .map_err(Problem::from)?
        .into_iter()
        .filter(|summary| {
            book_source_has_group(
                &BookSource {
                    book_source_group: summary.book_source_group.clone(),
                    ..BookSource::default()
                },
                INVALID_BOOK_SOURCE_GROUP,
            )
        })
        .filter_map(|summary| summary.book_source_id)
        .collect::<Vec<_>>();
    let mut deleted = 0u32;
    for id in invalid_ids {
        let removed = state
            .book_source_service
            .delete_by_id(&user.ns, id)
            .await
            .map_err(Problem::from)?;
        if removed {
            deleted += 1;
        }
    }
    Ok(Json(BatchDeleteSourcesResult { deleted }))
}
