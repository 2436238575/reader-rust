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
    let list = state.bookmark_repo.list(&user.0.ns).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(list).unwrap_or_default(),
    )))
}

pub async fn save_bookmark(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(bookmark): Json<Bookmark>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if bookmark.book_id.trim().is_empty() {
        return Err(AppError::BadRequest("bookId required".to_string()));
    }
    state.bookmark_repo.upsert(&user.0.ns, &bookmark).await?;
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

pub async fn save_bookmarks(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(bookmarks): Json<Vec<Bookmark>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    for b in &bookmarks {
        if !b.book_id.trim().is_empty() {
            state.bookmark_repo.upsert(&user.0.ns, b).await?;
        }
    }
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

pub async fn delete_bookmark(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(bookmark): Json<Bookmark>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    state
        .bookmark_repo
        .delete(
            &user.0.ns,
            &bookmark.book_id,
            bookmark.chapter_index,
            bookmark.chapter_pos,
            &bookmark.kind,
        )
        .await?;
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

pub async fn delete_bookmarks(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(bookmarks): Json<Vec<Bookmark>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    for b in &bookmarks {
        state
            .bookmark_repo
            .delete(
                &user.0.ns,
                &b.book_id,
                b.chapter_index,
                b.chapter_pos,
                &b.kind,
            )
            .await?;
    }
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

/// 删书联动：该书的书签一并删除（bookId 不再存在，书签留着也是孤儿）。
pub(crate) async fn delete_bookmarks_for_book(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
) -> Result<(), AppError> {
    state
        .bookmark_repo
        .delete_for_book(user_ns, book_id)
        .await?;
    Ok(())
}
