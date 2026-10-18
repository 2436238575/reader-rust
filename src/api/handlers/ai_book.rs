use axum::{
    body::Bytes,
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::Value;

use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::model::ai_book::AiBookMemory;

#[derive(Debug, Deserialize, Default)]
pub struct AiBookMemoryRequest {
    #[serde(rename = "bookId")]
    pub book_id: Option<String>,
}

pub async fn get_ai_book_memory(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<AiBookMemoryRequest>,
    body: Bytes,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let req = parse_ai_book_request(q, body)?;
    let book_id = required_book_id(req.book_id)?;
    ensure_shelf_book(&state, &user_ns, &book_id).await?;
    let mut memory = state.ai_book_service.get(&user_ns, &book_id).await?;
    if let Some(ref mut m) = memory {
        m.book_id = Some(book_id.clone());
    }
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(memory).unwrap_or_default(),
    )))
}

pub async fn save_ai_book_memory(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(mut memory): Json<AiBookMemory>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let book_id = memory
        .book_id
        .clone()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let shelf_book = ensure_shelf_book(&state, &user_ns, &book_id).await?;
    // bookUrl 是冗余数据（恢复校验用），以登记表当前绑定为准
    memory.book_url = shelf_book.book_url.clone();
    if memory.book_name.as_deref().unwrap_or("").trim().is_empty() {
        memory.book_name = Some(shelf_book.name);
    }
    if memory.author.as_deref().unwrap_or("").trim().is_empty() {
        memory.author = Some(shelf_book.author);
    }
    let mut saved = state
        .ai_book_service
        .save_for_book(&user_ns, &book_id, &shelf_book.book_url, memory)
        .await?;
    saved.book_id = Some(book_id);
    Ok(Json(ApiResponse::ok(
        serde_json::to_value(saved).unwrap_or_default(),
    )))
}

pub async fn delete_ai_book_memory(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<AiBookMemoryRequest>,
    body: Bytes,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let user_ns = user.0.ns.clone();
    let req = parse_ai_book_request(q, body)?;
    let book_id = required_book_id(req.book_id)?;
    ensure_shelf_book(&state, &user_ns, &book_id).await?;
    let deleted = state.ai_book_service.delete(&user_ns, &book_id).await?;
    Ok(Json(ApiResponse::ok(
        serde_json::json!({ "deleted": deleted }),
    )))
}

async fn ensure_shelf_book(
    state: &AppState,
    user_ns: &str,
    book_id: &str,
) -> Result<crate::model::book::Book, AppError> {
    state
        .book_service
        .get_shelf_book(user_ns, book_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("书籍未加入书架".to_string()))
}

fn parse_ai_book_request(
    q: AiBookMemoryRequest,
    body: Bytes,
) -> Result<AiBookMemoryRequest, AppError> {
    if body.is_empty() {
        return Ok(q);
    }
    if let Ok(v) = serde_json::from_slice::<AiBookMemoryRequest>(&body) {
        return Ok(v);
    }
    let text = std::str::from_utf8(&body).map_err(|e| AppError::BadRequest(e.to_string()))?;
    let mut req = q;
    for (k, v) in url::form_urlencoded::parse(text.as_bytes()) {
        if k.as_ref() == "bookId" {
            req.book_id = Some(v.into_owned());
        }
    }
    Ok(req)
}

fn required_book_id(book_id: Option<String>) -> Result<String, AppError> {
    book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))
}
