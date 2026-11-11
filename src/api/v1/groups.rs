//! `/v1/me/groups`：书架分组（`groupId` 是位域、分配 2 的幂，与阅读3.0
//! 的位掩码约定一致）。增删改的语义与 `/reader3` 一致：
//! - `POST`（upsert）：`groupId=0` 表示新建（分配空位幂），非 0 必须是合法幂位；
//! - `PATCH /:order`：**只改顺序**，不校验/不改名——增删改唯一入口是
//!   `POST` 与 `DELETE`（旧版整表覆盖会丢组，见 BookGroupOrderItem 注释）；
//! - `DELETE`：组行与书籍位掩码里的对应位一起清，空出的幂位可回填。
//!
//! 书籍的分组归属写在书架行上（`PATCH /v1/me/shelf/{bookId}` 的 `group`
//! 字段），这里只管组本身。

use crate::api::problem::Problem;
use crate::api::v1::{V1Json, V1User};
use crate::api::AppState;
use crate::model::book_group::{BookGroup, BookGroupOrderItem};
use crate::service::book_group_service::validate_group_id;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch};
use axum::{Json, Router};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/groups", get(list_groups).post(save_group))
        .route("/v1/me/groups:order", patch(reorder_groups))
        .route("/v1/me/groups/{groupId}", delete(delete_group))
}

/// 全部分组（按 orderNo 排序）。
#[utoipa::path(
    get,
    path = "/v1/me/groups",
    tag = "me",
    operation_id = "listBookGroups",
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "分组列表", body = [BookGroup]),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_groups(
    State(state): State<AppState>,
    V1User(user): V1User,
) -> Result<Json<Vec<BookGroup>>, Problem> {
    let groups = state
        .book_group_service
        .get_groups(&user.ns)
        .await
        .map_err(Problem::from)?;
    Ok(Json(groups))
}

/// 新建（`groupId=0`，服务端分配空位幂）或更新（非 0 合法幂位）。
#[utoipa::path(
    post,
    path = "/v1/me/groups",
    tag = "me",
    operation_id = "saveBookGroup",
    request_body = BookGroup,
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已保存"),
        (status = 400, description = "groupId 非法或分组已达上限", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn save_group(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(group): V1Json<BookGroup>,
) -> Result<StatusCode, Problem> {
    state
        .book_group_service
        .save_group(&user.ns, group)
        .await
        .map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 分组排序：只改 orderNo，未提交的分组保持原值（不删组、不改名）。
#[utoipa::path(
    patch,
    path = "/v1/me/groups:order",
    tag = "me",
    operation_id = "reorderBookGroups",
    request_body = [BookGroupOrderItem],
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已保存"),
        (status = 400, description = "空数组 / 重复 groupId / 非法幂位 / 组不存在", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn reorder_groups(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Json(items): V1Json<Vec<BookGroupOrderItem>>,
) -> Result<StatusCode, Problem> {
    state
        .book_group_service
        .reorder_groups(&user.ns, &items)
        .await
        .map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 删组：组行与书籍位掩码里的对应位一起清（幂等）。
#[utoipa::path(
    delete,
    path = "/v1/me/groups/{groupId}",
    tag = "me",
    operation_id = "deleteBookGroup",
    params(("groupId" = i64, Path, description = "分组 id（2 的幂）")),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 204, description = "已删除（幂等）"),
        (status = 400, description = "groupId 非法（非 2 的幂）", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_group(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(group_id): Path<i64>,
) -> Result<StatusCode, Problem> {
    validate_group_id(group_id).map_err(Problem::from)?;
    state
        .book_group_service
        .delete_group(&user.ns, group_id)
        .await
        .map_err(Problem::from)?;
    Ok(StatusCode::NO_CONTENT)
}
