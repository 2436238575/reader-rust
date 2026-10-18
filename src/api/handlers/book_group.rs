use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::book_group::BookGroup;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::Value;

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
