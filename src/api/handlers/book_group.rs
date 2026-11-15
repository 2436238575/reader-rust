use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::book_group::{BookGroup, BookGroupOrderItem};
use crate::service::book_group_service::validate_group_id;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::Value;

/// 入组/改组的公共校验：gid 必须是合法幂位且组存在。
/// 位掩码语义下，非法 gid（3 = 组 1|2）或悬空位会污染整张书架的过滤结果。
async fn ensure_group_for_membership(
    state: &AppState,
    user_ns: &str,
    group_id: i64,
) -> Result<(), AppError> {
    validate_group_id(group_id)?;
    if !state
        .book_group_service
        .group_exists(user_ns, group_id)
        .await?
    {
        return Err(AppError::BadRequest("分组不存在".to_string()));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct GroupIdParam {
    #[serde(rename = "groupId")]
    group_id: Option<i64>,
}

pub async fn get_book_groups(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let groups = state.book_group_service.get_groups(&user_ns).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(groups).unwrap_or_default(),
    )))
}

pub async fn save_book_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(group): Json<BookGroup>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    state.book_group_service.save_group(&user_ns, group).await?;
    Ok(Json(ApiResponse::ok(serde_json::json!("success"))))
}

pub async fn delete_book_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(param): Json<GroupIdParam>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let gid = param
        .group_id
        .ok_or_else(|| AppError::BadRequest("groupId required".to_string()))?;
    state.book_group_service.delete_group(&user_ns, gid).await?;
    Ok(Json(ApiResponse::ok(serde_json::json!("success"))))
}

/// 分组排序（`POST /reader3/saveBookGroupOrder`）。
///
/// 请求体 `[{"groupId": 1, "orderNo": 0}, …]`，**只改顺序**：
/// - 未提交的分组保持原 `orderNo`，不会被删除（旧版整表覆盖会删组并清书上的位）；
/// - 不校验/不改 `groupName`——增删改唯一入口是 `/saveBookGroup` 与 `/deleteBookGroup`；
/// - 空数组、重复 groupId、非法幂位、组不存在一律 400。
pub async fn save_book_group_order(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(items): Json<Vec<BookGroupOrderItem>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    state
        .book_group_service
        .reorder_groups(&user_ns, &items)
        .await?;
    Ok(Json(ApiResponse::ok(serde_json::json!("success"))))
}

#[derive(Debug, Deserialize)]
pub struct SaveBookGroupIdParam {
    #[serde(rename = "bookId")]
    book_id: Option<String>,
    #[serde(rename = "groupId")]
    group_id: Option<i64>,
}

pub async fn save_book_group_id(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(param): Json<SaveBookGroupIdParam>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let book_id = param
        .book_id
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let gid = param.group_id.unwrap_or(0);
    // 0 = 移出全部分组；其余必须是存在且合法的幂位（直接赋值不会做位运算，
    // 传 3 就等于让书同时进两个组）
    if gid != 0 {
        ensure_group_for_membership(&state, &user_ns, gid).await?;
    }
    let mut book = state
        .book_service
        .get_shelf_book(&user_ns, &book_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Book not found".to_string()))?;
    book.group = Some(gid);
    state.book_service.save_book(&user_ns, book).await?;
    Ok(Json(ApiResponse::ok(serde_json::json!("success"))))
}
