//! `GET /v1/books/{bookId}/source-candidates/events`：换源候选 SSE。
//!
//! 与 legacy `/reader3/getAvailableBookSourceSSE` 共用同一份探测实现
//! （`handlers::book::available_source::build_available_source_sse`），只是
//! 入口换成 v1、参数改成 typed query、鉴权走 V1User。事件帧沿用旧格式：
//! - 默认 `message`：`{lastIndex, hasMore, data:[SearchBook]}`
//! - `end`：`{lastIndex, hasMore}`
//!
//! 候选按 name/author 匹配、排除当前书源；`refresh=true` 跳过新鲜缓存，
//! `lastIndex` 用于「加载更多」续扫。SSE 是 GET，不需要 CSRF 头；浏览器
//! 由 Cookie 鉴权，Bearer 客户端可用 fetch 流读取。

use crate::api::handlers::book::available_source::build_available_source_sse;
use crate::api::problem::{ErrorCode, FieldIssue, Problem};
use crate::api::v1::{V1Query, V1User};
use crate::api::AppState;
use axum::extract::{Path, State};
use axum::response::sse::{Event, Sse};
use axum::routing::get;
use axum::Router;
use futures::Stream;
use serde::Deserialize;
use std::convert::Infallible;

const DEFAULT_CONCURRENT_COUNT: i32 = 8;
const MAX_CONCURRENT_COUNT: i32 = 20;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceCandidatesQuery {
    /// 忽略新鲜缓存，重新逐源搜索
    pub refresh: Option<bool>,
    /// 上一次事件的 lastIndex；缺省 -1（从头开始）
    pub last_index: Option<i32>,
    /// 单批并发源数 1..=20，缺省 8
    pub concurrent_count: Option<i32>,
}

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/v1/books/{bookId}/source-candidates/events",
        get(source_candidate_events),
    )
}

fn range_error(pointer: &str, detail: &str) -> Problem {
    Problem::new(ErrorCode::ValidationFailed)
        .detail(detail)
        .errors(vec![FieldIssue {
            pointer: pointer.to_string(),
            code: "RANGE".to_string(),
            detail: Some(detail.to_string()),
        }])
}

/// 换源候选 SSE：逐源搜索同名/同作者的书，命中即推送，`end` 收尾。
#[utoipa::path(
    get,
    path = "/v1/books/{bookId}/source-candidates/events",
    tag = "books",
    operation_id = "sourceCandidateEvents",
    params(
        ("bookId" = String, Path, description = "书籍 id（book_<n>）"),
        ("refresh" = Option<bool>, Query, description = "忽略新鲜缓存，缺省 false"),
        ("lastIndex" = Option<i32>, Query, description = "续扫游标，缺省 -1", minimum = -1),
        ("concurrentCount" = Option<i32>, Query, description = "单批并发源数 1..=20，缺省 8", minimum = 1, maximum = 20),
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "SSE：message {lastIndex,hasMore,data:[SearchBook]}，end {lastIndex,hasMore}", content_type = "text/event-stream", body = String),
        (status = 400, description = "查询参数类型非法", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "书籍不存在", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "lastIndex/concurrentCount 越界", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn source_candidate_events(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(book_id): Path<String>,
    V1Query(query): V1Query<SourceCandidatesQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Problem> {
    let last_index = query.last_index.unwrap_or(-1);
    if last_index < -1 {
        return Err(range_error("/lastIndex", "lastIndex 必须 >= -1"));
    }
    let concurrent_count = query.concurrent_count.unwrap_or(DEFAULT_CONCURRENT_COUNT);
    if !(1..=MAX_CONCURRENT_COUNT).contains(&concurrent_count) {
        return Err(range_error(
            "/concurrentCount",
            "concurrentCount 必须在 1..=20 之间",
        ));
    }
    // 只校验登记行存在，不要求当前绑定书源仍可解析：书源被删后正是最需要
    // 换源的场景，use resolve_book 会误报 404
    crate::api::v1::ensure_book_registered(&state, &user.ns, &book_id).await?;
    build_available_source_sse(
        state,
        user.ns,
        book_id,
        query.refresh.unwrap_or(false),
        last_index,
        concurrent_count as usize,
    )
    .await
    .map_err(Problem::from)
}
