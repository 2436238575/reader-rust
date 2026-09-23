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
    if bookmark.book_name.is_empty() && bookmark.book_author.is_empty() {
        return Err(AppError::BadRequest("书籍信息错误".to_string()));
    }
    let mut list = read_list::<Bookmark>(&state, &user_ns, "bookmark.json").await?;
    upsert_by_key(&mut list, bookmark, |b| {
        format!("{}_{}", b.book_name, b.book_author)
    });
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
    bookmarks.retain(|b| !(b.book_name.is_empty() && b.book_author.is_empty()));
    for b in bookmarks {
        upsert_by_key(&mut list, b, |v| {
            format!("{}_{}", v.book_name, v.book_author)
        });
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
    list.retain(|b| !(b.book_name == bookmark.book_name && b.book_author == bookmark.book_author));
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
        list.retain(|v| !(v.book_name == b.book_name && v.book_author == b.book_author));
    }
    write_list(&state, &user_ns, "bookmark.json", &list).await?;
    Ok(Json(ApiResponse::ok(Value::String("".to_string()))))
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

fn upsert_by_key<T, F>(list: &mut Vec<T>, item: T, key_fn: F)
where
    F: Fn(&T) -> String,
{
    let key = key_fn(&item);
    if let Some(pos) = list.iter().position(|v| key_fn(v) == key) {
        list[pos] = item;
    } else {
        list.push(item);
    }
}
