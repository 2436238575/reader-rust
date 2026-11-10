//! RFC 9457 problem+json：`/v1` 的唯一错误形态。
//!
//! `code` 给机器（前端 switch 分派，稳定不变、进 spec），`detail` 给人
//! （前端只塞 toast、绝不解析），`traceId` 与响应头 `X-Request-Id` 一致，
//! 用户截图能定位到日志。可选成员按 RFC 9457 惯例缺省省略——这条不受
//! 「资源 schema 字段始终出现」约束（那是给资源响应的；problem 的可选成员
//! 是协议标注，前端不依赖其存在性）。
//!
//! 通用兜底码（`NotFound` / `Conflict`）：`AppError` 到 `Problem` 的兜底
//! 映射无法知道具体资源，只能给通用码；领域 handler 落地时应构造带精确
//! 资源码的 `Problem`（如 `BookNotFound`），不要依赖兜底。

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

/// 错误码：线上值是大写下划线（serde `SCREAMING_SNAKE_CASE`），稳定不变。
/// 新增取值是兼容变更（客户端必须有 default 分支），改已有取值的语义是破坏变更。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// 字段校验失败，`errors[]` 带 JSON Pointer
    ValidationFailed,
    /// 请求体无法解析或结构非法
    MalformedRequest,
    /// 游标无法解码或已失效
    InvalidCursor,
    /// 用户名或密码错误（不区分两种情况）
    InvalidCredentials,
    /// 无凭据或 access token 过期/失效，客户端应静默刷新
    AuthExpired,
    /// refresh token 重放，会话已吊销，必须重新登录
    AuthRefreshReplayed,
    /// 会话失效（如账号行已不存在），必须重新登录；与 `AUTH_EXPIRED` 的
    /// 区别是静默刷新救不回来，前端不应尝试刷新
    SessionInvalid,
    /// 缺少 `X-Requested-With` 头（CSRF 双保险）
    CsrfHeaderMissing,
    /// 限定 scope 的令牌访问了白名单外路径
    ScopeForbidden,
    /// 书籍不存在
    BookNotFound,
    /// 章节不存在
    ChapterNotFound,
    /// 书源不存在
    SourceNotFound,
    /// 图片不存在
    ImageNotFound,
    /// 任务不存在
    TaskNotFound,
    /// 通用 404 兜底（领域 handler 应使用带资源的精确码）
    NotFound,
    /// 书籍未加入书架
    BookNotInShelf,
    /// 书源已禁用
    SourceDisabled,
    /// 同类任务已在执行
    TaskAlreadyRunning,
    /// 通用 409 兜底（领域 handler 应使用带语义的精确码）
    Conflict,
    /// `If-Match` 前置条件失败
    VersionConflict,
    /// 请求体超限
    PayloadTooLarge,
    /// `Content-Type` 不支持
    UnsupportedMediaType,
    /// 被限流，带 `Retry-After`
    RateLimited,
    /// 出站被私网策略拦截
    UpstreamBlocked,
    /// 上游站点失败，`detail` 带真实状态码
    UpstreamFailed,
    /// 上游超时
    UpstreamTimeout,
    /// 服务端处理超时（非 408）
    ProcessingTimeout,
    /// 并发任务超限
    TaskLimit,
    /// 旧接口已删除（`/reader3` 墓碑）
    ApiRemoved,
    /// 请求字段未知或不可写
    UnknownField,
    /// 服务端错误，不泄露细节，只给 traceId
    Internal,
}

impl ErrorCode {
    /// 与 HTTP 状态码一一对应；problem body 里的 `status` 与之一致。
    pub fn status(self) -> StatusCode {
        match self {
            Self::MalformedRequest | Self::UpstreamBlocked => StatusCode::BAD_REQUEST,
            // 契约口径：语法合法但字段值非法 → 422（cursor 无法解码同属此类）
            Self::ValidationFailed | Self::InvalidCursor | Self::UnknownField => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::InvalidCredentials
            | Self::AuthExpired
            | Self::AuthRefreshReplayed
            | Self::SessionInvalid => StatusCode::UNAUTHORIZED,
            Self::CsrfHeaderMissing | Self::ScopeForbidden => StatusCode::FORBIDDEN,
            Self::BookNotFound
            | Self::ChapterNotFound
            | Self::SourceNotFound
            | Self::ImageNotFound
            | Self::TaskNotFound
            | Self::NotFound => StatusCode::NOT_FOUND,
            Self::BookNotInShelf
            | Self::SourceDisabled
            | Self::TaskAlreadyRunning
            | Self::Conflict => StatusCode::CONFLICT,
            Self::VersionConflict => StatusCode::PRECONDITION_FAILED,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::RateLimited | Self::TaskLimit => StatusCode::TOO_MANY_REQUESTS,
            Self::UpstreamFailed | Self::UpstreamTimeout => StatusCode::BAD_GATEWAY,
            Self::ProcessingTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::ApiRemoved => StatusCode::GONE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// HTTP 状态的英文名（RFC 9457 的 `title` 惯例）；给人看的中文走 `detail`。
    pub fn title(self) -> &'static str {
        match self {
            Self::ValidationFailed => "Validation Failed",
            Self::MalformedRequest => "Malformed Request",
            Self::UnknownField => "Unknown Field",
            Self::InvalidCursor => "Invalid Cursor",
            Self::InvalidCredentials => "Invalid Credentials",
            Self::AuthExpired => "Auth Expired",
            Self::AuthRefreshReplayed => "Auth Refresh Replayed",
            Self::SessionInvalid => "Session Invalid",
            Self::CsrfHeaderMissing => "CSRF Header Missing",
            Self::ScopeForbidden => "Scope Forbidden",
            Self::BookNotFound => "Book Not Found",
            Self::ChapterNotFound => "Chapter Not Found",
            Self::SourceNotFound => "Source Not Found",
            Self::ImageNotFound => "Image Not Found",
            Self::TaskNotFound => "Task Not Found",
            Self::NotFound => "Not Found",
            Self::BookNotInShelf => "Book Not In Shelf",
            Self::SourceDisabled => "Source Disabled",
            Self::TaskAlreadyRunning => "Task Already Running",
            Self::Conflict => "Conflict",
            Self::VersionConflict => "Version Conflict",
            Self::PayloadTooLarge => "Payload Too Large",
            Self::UnsupportedMediaType => "Unsupported Media Type",
            Self::RateLimited => "Rate Limited",
            Self::UpstreamBlocked => "Upstream Blocked",
            Self::UpstreamFailed => "Upstream Failed",
            Self::UpstreamTimeout => "Upstream Timeout",
            Self::ProcessingTimeout => "Processing Timeout",
            Self::TaskLimit => "Task Limit",
            Self::ApiRemoved => "API Removed",
            Self::Internal => "Internal Server Error",
        }
    }
}

/// 422 校验失败的逐字段错误：`pointer` 是 JSON Pointer，前端直接映射表单控件。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FieldIssue {
    pub pointer: String,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub detail: Option<String>,
}

/// problem+json 响应体。`type` 固定 `about:blank`（RFC 9457 缺省类型，
/// 语义由 `code` 承载，不要求维护错误文档页）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    #[serde(rename = "type")]
    type_uri: String,
    code: ErrorCode,
    status: u16,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub(crate) trace_id: Option<String>,
    /// 上游业务码原样透传（FQWeb 的 `UPSTREAM_THROTTLED` 等）
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    upstream_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    errors: Option<Vec<FieldIssue>>,
    /// 429 的 `Retry-After` 秒数；只进响应头，不进 body
    #[serde(skip)]
    retry_after: Option<u64>,
}

impl Problem {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            type_uri: "about:blank".to_string(),
            code,
            status: code.status().as_u16(),
            title: code.title().to_string(),
            detail: None,
            instance: None,
            trace_id: None,
            upstream_code: None,
            errors: None,
            retry_after: None,
        }
    }

    /// 给人看的文案；前端只展示，不解析。
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self
    }

    pub fn trace(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    pub fn upstream_code(mut self, code: impl Into<String>) -> Self {
        self.upstream_code = Some(code.into());
        self
    }

    pub fn errors(mut self, errors: Vec<FieldIssue>) -> Self {
        self.errors = Some(errors);
        self
    }

    /// 只读访问：批量接口把逐项错误回表时用（`Problem` 本身是 HTTP 出口，
    /// 逐项错误只需要 code + detail）
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn detail_text(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// 429 的 `Retry-After` 秒数（进响应头）。
    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let retry_after = self.retry_after;
        let mut response = Json(self).into_response();
        *response.status_mut() = status;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/problem+json"),
        );
        // 429 的 Retry-After：只进响应头，不进 body
        if let Some(secs) = retry_after {
            if let Ok(value) = header::HeaderValue::from_str(&secs.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

impl From<crate::error::error::AppError> for Problem {
    /// `AppError` 的兜底映射。领域 handler 对错误码有精确要求时应显式构造
    /// `Problem`（如换掉 `NotFound` 兜底），这里只保证形态正确与文案脱敏。
    fn from(error: crate::error::error::AppError) -> Self {
        use crate::error::error::AppError;
        match error {
            AppError::Unauthorized(_) => Self::new(ErrorCode::AuthExpired),
            AppError::BadRequest(message) => Self::new(ErrorCode::MalformedRequest).detail(message),
            AppError::Blocked(message) => Self::new(ErrorCode::UpstreamBlocked).detail(message),
            AppError::NotFound(_) => Self::new(ErrorCode::NotFound),
            AppError::Conflict(message) => Self::new(ErrorCode::Conflict).detail(message),
            AppError::TooManyRequests {
                message,
                retry_after_secs,
            } => Self::new(ErrorCode::RateLimited)
                .detail(message)
                .retry_after(retry_after_secs),
            AppError::Upstream { status, message } => {
                let code = if status == 408 {
                    ErrorCode::UpstreamTimeout
                } else {
                    ErrorCode::UpstreamFailed
                };
                Self::new(code).detail(message)
            }
            // Db/Internal/Http 一律脱敏：内部细节只进日志，对外只给笼统文案 + traceId
            other => Self::new(ErrorCode::Internal).detail(other.client_message()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_serializes_camel_case_and_omits_absent_members() {
        let body = serde_json::to_value(
            Problem::new(ErrorCode::BookNotInShelf)
                .detail("这本书还没有加入书架")
                .trace("01J8XQ"),
        )
        .unwrap();
        assert_eq!(body["type"], "about:blank");
        assert_eq!(body["code"], "BOOK_NOT_IN_SHELF");
        assert_eq!(body["status"], 409);
        assert_eq!(body["title"], "Book Not In Shelf");
        assert_eq!(body["detail"], "这本书还没有加入书架");
        assert_eq!(body["traceId"], "01J8XQ");
        assert!(body.get("instance").is_none(), "缺省成员不出现");
        assert!(body.get("upstreamCode").is_none());
        assert!(body.get("errors").is_none());
    }

    #[test]
    fn app_error_fallback_mapping_is_sanitized() {
        let internal = Problem::from(crate::error::error::AppError::Internal(anyhow::anyhow!(
            "数据库密码泄漏细节"
        )));
        assert_eq!(internal.status, 500);
        assert_eq!(internal.code, ErrorCode::Internal);
        assert!(
            !internal
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("数据库密码"),
            "Internal 不得泄露内部细节"
        );
        assert_eq!(
            Problem::from(crate::error::error::AppError::Unauthorized(
                "需要登录".into()
            ))
            .code,
            ErrorCode::AuthExpired
        );
        assert_eq!(
            Problem::from(crate::error::error::AppError::NotFound("书不存在".into())).code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn error_code_status_table_matches_contract() {
        assert_eq!(ErrorCode::AuthExpired.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            ErrorCode::VersionConflict.status(),
            StatusCode::PRECONDITION_FAILED
        );
        assert_eq!(ErrorCode::UpstreamFailed.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            ErrorCode::ProcessingTimeout.status(),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(ErrorCode::ApiRemoved.status(), StatusCode::GONE);
        assert_eq!(ErrorCode::TaskLimit.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
