//! `GET /v1/me`：当前账号资料。垂直切片端点——打通 utoipa derive →
//! openapi 生成 → problem+json → 具名响应 schema 的整条管线。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::v1::V1User;
use crate::api::AppState;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use utoipa::ToSchema;

/// 当前账号资料。`id` 是不透明字符串（`u_<uid>`），客户端不解析。
/// `createdAt` 一律 RFC 3339 UTC（契约红线 7）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub id: String,
    pub username: String,
    /// 注册时间，RFC 3339 UTC
    #[schema(format = DateTime)]
    pub created_at: String,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/me", get(get_me))
}

/// 当前账号资料。
///
/// 时间字段统一由服务端下发，客户端时钟不参与任何比较（契约 §5.2）。
#[utoipa::path(
    get,
    path = "/v1/me",
    tag = "me",
    operation_id = "getMe",
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "当前账号资料", body = Me),
        (status = 401, description = "未登录或令牌失效，客户端应静默刷新或重新登录",
         body = Problem, content_type = "application/problem+json"),
        (status = 500, description = "服务端错误",
         body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_me(
    State(state): State<AppState>,
    V1User(user): V1User,
) -> Result<Json<Me>, Problem> {
    let profile = state
        .user_service
        .get_profile(user.uid)
        .await
        .map_err(Problem::from)?
        // 令牌有效但账号行不存在：静默刷新救不回来，必须重新登录
        .ok_or_else(|| {
            Problem::new(ErrorCode::AuthRefreshReplayed).detail("账号不存在，请重新登录")
        })?;
    Ok(Json(Me {
        id: format!("u_{}", user.uid),
        username: profile.0,
        created_at: rfc3339_from_millis(profile.1),
    }))
}

/// Unix 毫秒 → RFC 3339 UTC（秒级精度，契约 §5.2「秒级足够」）。
/// users 表的时间戳是毫秒；0（未记录）同样渲染为 epoch。
fn rfc3339_from_millis(millis: i64) -> String {
    rfc3339_secs(millis.div_euclid(1000))
}

/// Unix 秒 → RFC 3339 UTC（秒级精度）。
fn rfc3339_secs(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).expect("epoch is valid"))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_formats_utc_seconds() {
        assert_eq!(rfc3339_secs(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_secs(1_762_557_600), "2025-11-07T23:20:00Z");
        // users.created_at 是毫秒（与库内 users 表口径一致）
        assert_eq!(rfc3339_from_millis(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339_from_millis(1_762_557_600_000),
            "2025-11-07T23:20:00Z"
        );
    }
}
