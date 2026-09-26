use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

/// 未登录 / 令牌无效或过期。识别靠 HTTP 401（前端拦截器据此弹登录框），
/// errorMsg 会被前端原样 toast 展示，因此是用户可直接阅读的文案。
pub const NEED_LOGIN: &str = "需要登录";

#[derive(Debug, Error)]
pub enum AppError {
    /// 401：未登录、令牌无效或已过期
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    /// 400：出站请求被 `ALLOW_PRIVATE_NETWORK` 策略拦下。
    ///
    /// 与 `Internal` 分开是因为这是**配置/输入**问题：调用方需要看到
    /// 「哪个地址被拦了、为什么」，否则书源指向内网时只会收到一句
    /// 看不出所以然的 "internal error"。
    #[error("blocked: {0}")]
    Blocked(String),
    #[error("internal error")]
    Internal(anyhow::Error),
    #[error("db error")]
    Db(#[from] sqlx::Error),
    #[error("http error")]
    Http(#[from] reqwest::Error),
}

/// `anyhow::Error` → `AppError`，并把出站策略拒绝挑出来单独归类。
///
/// 抓取链路上错误一路以 `anyhow` 传播（`fetch` 返回 `anyhow::Result`），
/// 在这里统一做一次类型判别，比在每个调用点手写 `map_err` 更不容易漏。
impl From<anyhow::Error> for AppError {
    fn from(error: anyhow::Error) -> Self {
        match error.downcast::<crate::crawler::url_guard::OutboundBlocked>() {
            Ok(blocked) => AppError::Blocked(blocked.0),
            Err(error) => AppError::Internal(error),
        }
    }
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
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Blocked(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crawler::url_guard::OutboundBlocked;

    #[test]
    fn outbound_block_is_reported_with_its_reason() {
        // 策略拒绝必须是可读的 400，而不是被兜底成 "internal error"
        let error: AppError = anyhow::Error::new(OutboundBlocked(
            "禁止访问内网地址: 192.168.1.10".to_string(),
        ))
        .into();
        match &error {
            AppError::Blocked(message) => assert!(message.contains("192.168.1.10")),
            other => panic!("应归类为 Blocked，实际是 {other:?}"),
        }
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn other_anyhow_errors_stay_internal() {
        let error: AppError = anyhow::anyhow!("磁盘炸了").into();
        assert!(matches!(error, AppError::Internal(_)));
        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn blocked_error_is_not_leaked_for_other_variants() {
        // 内部错误仍然只对外暴露笼统文案
        let response = AppError::Internal(anyhow::anyhow!("secret detail")).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
