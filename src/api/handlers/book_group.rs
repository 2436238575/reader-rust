use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::book_group::BookGroup;
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

pub async fn save_book_group_order(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(groups): Json<Vec<BookGroup>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    state
        .book_group_service
        .save_groups(&user_ns, &groups)
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

#[derive(Debug, Deserialize)]
pub struct MultiBookGroupParam {
    #[serde(rename = "bookIds")]
    book_ids: Option<Vec<String>>,
    #[serde(rename = "groupId")]
    group_id: Option<i64>,
}

pub async fn add_book_group_multi(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(param): Json<MultiBookGroupParam>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let ids = param.book_ids.unwrap_or_default();
    let gid = param.group_id.unwrap_or(0);
    // 入组必须落到一个真实存在的合法幂位上（挂空位会让目录过滤出幽灵分组）
    ensure_group_for_membership(&state, &user_ns, gid).await?;
    // 位域语义：`book.group = book.group | groupId`
    for id in ids {
        if let Some(mut book) = state.book_service.get_shelf_book(&user_ns, &id).await? {
            let cur = book.group.unwrap_or(0);
            book.group = Some(cur | gid);
            let _ = state.book_service.save_book(&user_ns, book).await;
        }
    }
    Ok(Json(ApiResponse::ok(serde_json::json!("success"))))
}

pub async fn remove_book_group_multi(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(param): Json<MultiBookGroupParam>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let ids = param.book_ids.unwrap_or_default();
    let gid = param.group_id.unwrap_or(0);
    // 出组只校验幂位（组行可能已被删，清残留位是合法操作）：传 3 会把
    // 组 1|2 两个位一起清掉，传 -1 直接清空全部组
    validate_group_id(gid)?;
    // remove: book.group = book.group & ~groupId
    for id in ids {
        if let Some(mut book) = state.book_service.get_shelf_book(&user_ns, &id).await? {
            let cur = book.group.unwrap_or(0);
            book.group = Some(cur & !gid);
            let _ = state.book_service.save_book(&user_ns, book).await;
        }
    }
    Ok(Json(ApiResponse::ok(serde_json::json!("success"))))
}
