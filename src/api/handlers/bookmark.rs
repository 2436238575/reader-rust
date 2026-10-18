use crate::api::AppState;
use crate::auth::CurrentUser;
use axum::{extract::State, Json};
use serde_json::Value;

use crate::error::error::{ApiResponse, AppError};
use crate::model::bookmark::Bookmark;

pub async fn get_bookmarks(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let list = read_list::<Bookmark>(&state, &user_ns, "bookmark.json").await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(list).unwrap_or_default(),
    )))
}

pub async fn save_bookmark(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(bookmark): Json<Bookmark>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    if bookmark.book_id.trim().is_empty() {
        return Err(AppError::BadRequest("bookId required".to_string()));
    }
    let mut list = read_list::<Bookmark>(&state, &user_ns, "bookmark.json").await?;
    upsert_bookmark(&mut list, bookmark);
    write_list(&state, &user_ns, "bookmark.json", &list).await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn save_bookmarks(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(mut bookmarks): Json<Vec<Bookmark>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let mut list = read_list::<Bookmark>(&state, &user_ns, "bookmark.json").await?;
    bookmarks.retain(|b| !b.book_id.trim().is_empty());
    for b in bookmarks {
        upsert_bookmark(&mut list, b);
    }
    write_list(&state, &user_ns, "bookmark.json", &list).await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn delete_bookmark(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(bookmark): Json<Bookmark>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let mut list = read_list::<Bookmark>(&state, &user_ns, "bookmark.json").await?;
    let identity = bookmark.identity();
    list.retain(|b| b.identity() != identity);
    write_list(&state, &user_ns, "bookmark.json", &list).await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

pub async fn delete_bookmarks(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(bookmarks): Json<Vec<Bookmark>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let mut list = read_list::<Bookmark>(&state, &user_ns, "bookmark.json").await?;
    for b in bookmarks {
        let identity = b.identity();
        list.retain(|v| v.identity() != identity);
    }
    write_list(&state, &user_ns, "bookmark.json", &list).await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
}

/// 删书联动：该书的书签一并删除（bookId 不再存在，书签留着也是孤儿）。
pub(crate) async fn delete_bookmarks_for_book(state: &AppState, user_ns: &str, book_id: &str) {
    if let Ok(mut list) = read_list::<Bookmark>(state, user_ns, "bookmark.json").await {
        let before = list.len();
        list.retain(|b| b.book_id != book_id);
        if list.len() != before {
            let _ = write_list(state, user_ns, "bookmark.json", &list).await;
        }
    }
}

async fn read_list<T: for<'de> serde::Deserialize<'de>>(
    state: &AppState,
    user_ns: &str,
    name: &str,
) -> Result<Vec<T>, AppError> {
    state.json_document_service.read_list(user_ns, name).await
}

async fn write_list<T: serde::Serialize>(
    state: &AppState,
    user_ns: &str,
    name: &str,
    list: &Vec<T>,
) -> Result<(), AppError> {
    state
        .json_document_service
        .write_list(user_ns, name, list)
        .await
}

fn upsert_bookmark(list: &mut Vec<Bookmark>, item: Bookmark) {
    let identity = item.identity();
    if let Some(pos) = list.iter().position(|v| v.identity() == identity) {
        list[pos] = item;
    } else {
        list.push(item);
    }
}
