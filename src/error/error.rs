use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

/// 未登录 / 令牌无效或过期。前端据此弹出登录框。
pub const NEED_LOGIN: &str = "NEED_LOGIN";
/// 已登录但权限不足。
pub const FORBIDDEN: &str = "FORBIDDEN";

#[derive(Debug, Error)]
pub enum AppError {
    /// 401：未登录、令牌无效或已过期
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// 403：已认证但无权访问
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
    #[error("db error")]
    Db(#[from] sqlx::Error),
    #[error("http error")]
    Http(#[from] reqwest::Error),
}

#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    #[serde(rename = "isSuccess")]
    pub is_success: bool,
    #[serde(rename = "errorMsg")]
    pub error_msg: String,
    pub data: Option<T>,
}

impl<T> ApiResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            is_success: true,
            error_msg: "".to_string(),
            data: Some(data),
        }
    }
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            is_success: false,
            error_msg: message.into(),
            data: None,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg.clone()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Db(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error".to_string(),
            ),
            AppError::Http(_) => (StatusCode::BAD_GATEWAY, "upstream error".to_string()),
            AppError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error".to_string(),
            ),
        };
        // 走 tracing 而非 println!：可被统一收集/过滤，也避免向 stdout 倾倒。
        // 4xx 是调用方的问题，warn 即可；一律 error! 会把「未登录」这类
        // 日常事件淹没在错误日志里，掩盖真正的服务端故障。
        if status.is_server_error() {
            tracing::error!(error = ?self, "请求处理失败");
        } else {
            tracing::warn!(error = ?self, "请求被拒绝");
        }
        let body = Json(ApiResponse::<serde_json::Value>::err(message));
        (status, body).into_response()
    }
}
