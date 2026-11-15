//! `/v1/me` 阅读统计：明细事件写入与派生统计读取。
//!
//! - `POST /v1/me/read-events:batch`：客户端按章上报「当日累计秒数」，
//!   服务端 upsert 取 MAX；同一事务推进书架 `dur_chapter_time`。书不在
//!   登记表时整批 404，不做部分成功。
//! - `GET /v1/me/stats`：总时长/今日时长/已读章数/读完本数/连续天数，
//!   全部从明细表派生，不落聚合行。
//!
//! 天边界由服务层按服务器固定时区偏移计算（本地 04:00 起算新一天）；
//! 响应里的 `dayKey` 供客户端校准本地累计值的清零时机。

use crate::api::problem::{ErrorCode, FieldIssue, Problem};
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use crate::error::error::AppError;
use crate::service::read_stats_service::{ReadEventInput, ReadEventsBatchResult, ReadingStats};
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use utoipa::ToSchema;

/// 单批事件上限：客户端把离线缓冲按此分片。
const MAX_READ_EVENTS_PER_BATCH: usize = 100;
/// 单章单日最多计 24 小时；超出按上限截断，避免异常客户端把总量打爆。
const MAX_SECONDS_PER_CHAPTER_DAY: i64 = 86_400;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/read-events:batch", post(save_read_events))
        .route("/v1/me/stats", get(get_reading_stats))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadEventItem {
    /// 书籍 id（`book_<n>`；别名会在服务端归一）
    pub book_id: String,
    /// 章节序号（≥0）
    #[schema(minimum = 0)]
    pub chapter_index: i32,
    /// 该章当日的累计阅读秒数；服务端按 0..=86400 截断
    pub seconds: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadEventsBatchRequest {
    /// 最多 100 条；空数组合法（用于只取服务端 dayKey）
    #[schema(max_items = 100)]
    pub events: Vec<ReadEventItem>,
    /// 客户端累计值所属的服务端 dayKey（上次响应拿到的值）。与当前日不同时
    /// 事件写回该历史日（跨日/离线缓冲），响应始终返回当前 dayKey；
    /// 缺省按当前日写入。
    pub day_key: Option<i64>,
}

fn field_error(pointer: String, code: &str, detail: &str) -> Problem {
    Problem::new(ErrorCode::ValidationFailed)
        .detail(detail)
        .errors(vec![FieldIssue {
            pointer,
            code: code.to_string(),
            detail: Some(detail.to_string()),
        }])
}

/// 批量上报阅读事件（按章当日累计秒数，服务端取 MAX）。
#[utoipa::path(
    post,
    path = "/v1/me/read-events:batch",
    tag = "me",
    operation_id = "saveReadEvents",
    request_body = ReadEventsBatchRequest,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "本批处理条数与服务端当前 dayKey", body = ReadEventsBatchResult),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "事件中的书籍不在登记表", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "事件超过 100 条、chapterIndex 为负数或字段非法", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn save_read_events(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(request): V1Json<ReadEventsBatchRequest>,
) -> Result<Json<ReadEventsBatchResult>, Problem> {
    if request.events.len() > MAX_READ_EVENTS_PER_BATCH {
        return Err(field_error(
            "/events".to_string(),
            "TOO_MANY",
            &format!("一次最多上报 {MAX_READ_EVENTS_PER_BATCH} 条阅读事件"),
        ));
    }
    let client_day_key = request.day_key;
    let mut events = Vec::with_capacity(request.events.len());
    for (index, event) in request.events.into_iter().enumerate() {
        if event.chapter_index < 0 {
            return Err(field_error(
                format!("/events/{index}/chapterIndex"),
                "OUT_OF_RANGE",
                "chapterIndex 不能为负数",
            ));
        }
        if event.book_id.trim().is_empty() {
            return Err(field_error(
                format!("/events/{index}/bookId"),
                "REQUIRED",
                "bookId 不能为空",
            ));
        }
        events.push(ReadEventInput {
            book_id: event.book_id,
            chapter_index: event.chapter_index,
            seconds: event.seconds.clamp(0, MAX_SECONDS_PER_CHAPTER_DAY),
        });
    }
    state
        .read_stats_service
        .record_batch(&user.ns, &events, client_day_key)
        .await
        .map(Json)
        .map_err(|error| match error {
            AppError::NotFound(detail) => Problem::new(ErrorCode::BookNotFound).detail(detail),
            other => Problem::from(other),
        })
}

/// 当前用户的阅读统计（全部由明细表派生）。
#[utoipa::path(
    get,
    path = "/v1/me/stats",
    tag = "me",
    operation_id = "getReadingStats",
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "累计/今日时长、已读章数、读完本数与连续天数", body = ReadingStats),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_reading_stats(
    State(state): State<AppState>,
    V1User(user): V1User,
) -> Result<Json<ReadingStats>, Problem> {
    state
        .read_stats_service
        .stats(&user.ns)
        .await
        .map(Json)
        .map_err(Problem::from)
}
