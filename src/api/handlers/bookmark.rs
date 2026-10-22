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
    // 书签必须挂在已登记的书上：book_url 冗余列是恢复校验的唯一依据，
    // 允许空值就等于放过「id 复用时挂到新书」的漏洞
    let book_url = state
        .book_service
        .book_repo()
        .get(&user.0.ns, &bookmark.book_id)
        .await?
        .map(|row| row.book_url)
        .ok_or_else(|| AppError::NotFound("书籍不存在".to_string()))?;
    state
        .bookmark_repo
        .upsert(&user.0.ns, &bookmark, &book_url)
        .await?;
    Ok(Json(ApiResponse::ok(Value::String(String::new()))))
}

pub async fn save_bookmarks(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(bookmarks): Json<Vec<Bookmark>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    // 批量恢复（WebDAV）时逐条查登记表的 book_url；同书多条只查一次
    let mut url_cache: std::collections::HashMap<String, Option<String>> = Default::default();
    let mut items: Vec<(Bookmark, String)> = Vec::with_capacity(bookmarks.len());
    let mut skipped = 0usize;
    for b in bookmarks {
        if b.book_id.trim().is_empty() {
            continue;
        }
        let book_url = match url_cache.get(&b.book_id) {
            Some(url) => url.clone(),
            None => {
                let url = state
                    .book_service
                    .book_repo()
                    .get(&user.0.ns, &b.book_id)
                    .await?
                    .map(|row| row.book_url);
                url_cache.insert(b.book_id.clone(), url.clone());
                url
            }
        };
        match book_url {
            Some(url) => items.push((b, url)),
            // 备份里的书签指向不在本次备份书架里的书：没有归属信息，
            // 存进去只会在 id 复用时串书，跳过并留日志
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            "saveBookmarks: skipped {skipped} bookmark(s) whose books are not registered"
        );
    }
    // 单事务落库：中途失败整体回滚，不留半批书签
    state.bookmark_repo.upsert_many(&user.0.ns, &items).await?;
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
