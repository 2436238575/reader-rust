//! 本地书上传（TXT / EPUB）。EPUB 书内资源访问在 `/v1/local-books/assets`。

use super::shelf::ensure_user_local_book_limit;
use crate::api::handlers::multipart::read_limited_multipart_field;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::service::local_epub_book::MAX_EPUB_UPLOAD_BYTES;
use crate::service::local_txt_book::MAX_TXT_UPLOAD_BYTES;
use axum::{
    extract::{Multipart, State},
    Json,
};
use serde_json::Value;

pub async fn upload_txt_book(
    State(state): State<AppState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let mut file_name = String::new();
    let mut bytes: Option<Vec<u8>> = None;
    let mut field_count = 0usize;
    while let Some(field) =
        crate::api::handlers::multipart::next_field_limited(&mut multipart, &mut field_count)
            .await?
    {
        let field_name = field.name().unwrap_or_default().to_string();
        if field_name != "file" {
            continue;
        }
        file_name = field.file_name().unwrap_or("book.txt").to_string();
        bytes = Some(
            read_limited_multipart_field(field, MAX_TXT_UPLOAD_BYTES, "TXT 文件不能超过 50MB")
                .await?,
        );
        break;
    }

    let bytes = bytes.ok_or_else(|| AppError::BadRequest("file required".to_string()))?;
    let book = state
        .local_txt_book_service
        .import_txt_book(&user_ns, &file_name, &bytes)
        .await?;
    if let Err(err) = ensure_user_local_book_limit(&state, &user_ns, &book.book_url).await {
        if let Err(cleanup_err) = state
            .local_txt_book_service
            .delete_book_files(&user_ns, &book.book_url)
            .await
        {
            tracing::warn!(
                "failed to clean up rejected local txt book {}: {:?}",
                book.book_url,
                cleanup_err
            );
        }
        return Err(err);
    }
    let saved = state.book_service.save_book(&user_ns, book).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(saved).unwrap_or_default(),
    )))
}

pub async fn upload_epub_book(
    State(state): State<AppState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();

    let mut file_name = String::new();
    let mut bytes: Option<Vec<u8>> = None;
    let mut field_count = 0usize;
    while let Some(field) =
        crate::api::handlers::multipart::next_field_limited(&mut multipart, &mut field_count)
            .await?
    {
        let field_name = field.name().unwrap_or_default().to_string();
        if field_name != "file" {
            continue;
        }
        file_name = field.file_name().unwrap_or("book.epub").to_string();
        bytes = Some(
            read_limited_multipart_field(field, MAX_EPUB_UPLOAD_BYTES, "EPUB 文件不能超过 80MB")
                .await?,
        );
        break;
    }

    let bytes = bytes.ok_or_else(|| AppError::BadRequest("file required".to_string()))?;
    let book = state
        .local_epub_book_service
        .import_epub_book(&user_ns, &file_name, bytes)
        .await?;
    if let Err(err) = ensure_user_local_book_limit(&state, &user_ns, &book.book_url).await {
        if let Err(cleanup_err) = state
            .local_epub_book_service
            .delete_book_files(&user_ns, &book.book_url)
            .await
        {
            tracing::warn!(
                "failed to clean up rejected local epub book {}: {:?}",
                book.book_url,
                cleanup_err
            );
        }
        return Err(err);
    }
    let saved = state.book_service.save_book(&user_ns, book).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(saved).unwrap_or_default(),
    )))
}
