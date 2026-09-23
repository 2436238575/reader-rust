//! multipart 表单的共享读取工具。

use crate::error::error::AppError;
use axum::extract::multipart::Field;

/// 流式读取单个 multipart 字段并在超过 `max_bytes` 时中止。
///
/// axum 的 `DefaultBodyLimit` 不覆盖 multipart 字段，`Field::bytes()/text()`
/// 会把整个字段无界读入内存，必须走逐块计数。
pub(crate) async fn read_limited_multipart_field(
    mut field: Field<'_>,
    max_bytes: usize,
    too_large_message: &str,
) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(AppError::BadRequest(too_large_message.to_string()));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
