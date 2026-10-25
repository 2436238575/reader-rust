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
    /// 409：已有同类任务进行中（AI 资料批量更新为单任务串行）。
    #[error("conflict: {0}")]
    Conflict(String),
    /// 502：上游站点失败——返回了错误状态码（403/404/限流等），或网络层
    /// 超时/连接失败。
    ///
    /// 与 `Internal` 分开是因为这是**上游站点**的问题：用户需要看到
    /// 「上游 403」这样的真实原因，否则书源被风控时只会看到一句
    /// "internal error"，还以为书本身没内容。
    #[error("upstream status {status}: {message}")]
    Upstream {
        /// 上游状态码；网络层失败（没有响应）时为 0
        status: u16,
        /// 用户可读文案（直接进 errorMsg 被前端 toast）
        message: String,
    },
    #[error("internal error")]
    Internal(anyhow::Error),
    #[error("db error")]
    Db(#[from] sqlx::Error),
    #[error("http error")]
    Http(#[from] reqwest::Error),
}

/// `anyhow::Error` → `AppError`，并把出站策略拒绝与上游网络错误挑出来单独归类。
///
/// 抓取链路上错误一路以 `anyhow` 传播（`fetch` 返回 `anyhow::Result`），
/// 在这里统一做一次类型判别，比在每个调用点手写 `map_err` 更不容易漏。
/// reqwest 错误的 Display 会带完整 URL（可能含敏感参数），对外文案只给
/// 泛化原因，细节留在服务端日志。
impl From<anyhow::Error> for AppError {
    fn from(error: anyhow::Error) -> Self {
        match error.downcast::<crate::crawler::url_guard::OutboundBlocked>() {
            Ok(blocked) => AppError::Blocked(blocked.0),
            Err(error) => match error.downcast::<reqwest::Error>() {
                Ok(err) => {
                    let status = err.status().map(|s| s.as_u16()).unwrap_or(0);
                    let message = if err.is_timeout() {
                        "上游站点连接超时".to_string()
                    } else if err.is_connect() {
                        "无法连接上游站点".to_string()
                    } else if status > 0 {
                        format!("上游请求失败（HTTP {status}）")
                    } else {
                        // DNS/重定向/解码错误等没有响应状态的场景：不带「HTTP 0」
                        "上游请求失败".to_string()
                    };
                    AppError::Upstream { status, message }
                }
                Err(error) => AppError::Internal(error),
            },
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

impl AppError {
    /// 写日志用的脱敏摘要。
    ///
    /// 不能 `?self` 整个打印：Http 变体的 reqwest 错误 Display/Debug 会带
    /// 完整 URL，上游图片/正文地址里的时效签名（`x-signature`）与登录态
    /// 参数会随之落进日志文件。Http 变体只给错误类别 + 去掉查询串/片段的
    /// URL；其余变体的 Display 本就是面向客户端的安全文案，原样使用。
    fn log_summary(&self) -> String {
        match self {
            AppError::Http(err) => {
                let category = if err.is_timeout() {
                    "timeout"
                } else if err.is_connect() {
                    "connect"
                } else if err.is_redirect() {
                    "redirect"
                } else if err.is_decode() {
                    "decode"
                } else if err.is_body() {
                    "body"
                } else if err.is_status() {
                    "status"
                } else if err.is_request() {
                    "request"
                } else if err.is_builder() {
                    "builder"
                } else {
                    "other"
                };
                match err.url() {
                    Some(url) => format!(
                        "http error ({category}): {}",
                        crate::crawler::url_guard::redact_url_for_log(url.as_str())
                    ),
                    None => format!("http error ({category})"),
                }
            }
            other => other.to_string(),
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
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::Upstream { message, .. } => (StatusCode::BAD_GATEWAY, message.clone()),
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
        // 日志字段用脱敏摘要而不是 ?self：reqwest 错误会带完整 URL。
        if status.is_server_error() {
            tracing::error!(error = %self.log_summary(), "请求处理失败");
        } else {
            tracing::warn!(error = %self.log_summary(), "请求被拒绝");
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

    #[test]
    fn upstream_status_is_reported_with_readable_message() {
        // 上游 403 必须是可读的 502 + 真实状态码，而不是 "internal error"
        let error = AppError::Upstream {
            status: 403,
            message: "上游站点返回错误（HTTP 403）".to_string(),
        };
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn reqwest_errors_in_anyhow_become_upstream_without_leaking_url() {
        // 抓取链路（anyhow）里的网络错误归 Upstream；reqwest 的 Display
        // 会带完整 URL（可能含敏感参数），对外只给泛化文案。
        // reqwest::Error 没有公开构造器，用无效 URL 的 build 失败制造一个。
        let err = reqwest::Client::new()
            .get("ht tp://invalid url")
            .build()
            .unwrap_err();
        let error: AppError = anyhow::Error::new(err).into();
        assert!(matches!(error, AppError::Upstream { .. }));
    }

    #[tokio::test]
    async fn http_error_log_summary_redacts_url_query() {
        // 连接一个必然失败的本地端口制造带 URL 的 reqwest 错误：
        // 日志摘要只保留类别与去查询串的地址，签名参数不落日志
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/pic?x-signature=secret&x-expires=9")
            .send()
            .await
            .unwrap_err();
        let summary = AppError::Http(err).log_summary();
        assert!(
            !summary.contains("x-signature") && !summary.contains("secret"),
            "摘要不得带查询串: {summary}"
        );
        assert!(summary.contains("http error"), "应带错误类别: {summary}");
        assert!(
            summary.contains("127.0.0.1:1/pic"),
            "应保留主机与路径: {summary}"
        );
    }

    #[test]
    fn non_http_variants_log_display_text() {
        let error = AppError::Upstream {
            status: 403,
            message: "上游站点返回错误（HTTP 403）".to_string(),
        };
        assert!(error.log_summary().contains("403"));
        assert_eq!(
            AppError::NotFound("没了".to_string()).log_summary(),
            "not found: 没了"
        );
    }
}
