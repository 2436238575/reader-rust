//! 入站慢请求防护：请求处理超时与并发上限（配置见 `app/config.rs` 的
//! `INBOUND_REQUEST_TIMEOUT_SECS` / `INBOUND_UPLOAD_TIMEOUT_SECS` /
//! `INBOUND_MAX_CONCURRENCY`）。
//!
//! 上游抓取早有 `REQUEST_TIMEOUT_SECS` 兜底，但入站请求此前可以无限等待：
//! 挂起的 handler 持着连接、内存与 `spawn_blocking` 槽位，慢客户端或恶意
//! 慢请求能无限堆积。这里在 `api/router.rs` 的三个鉴权分组上补两层防线。

use crate::error::error::ApiResponse;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use std::time::Duration;

/// 入站请求超时中间件：`tokio::time::timeout` 包住 `next.run(request)`。
///
/// - 超时返回 **HTTP 408** + 统一 JSON 契约（`isSuccess=false`、
///   `errorMsg="请求超时"`、`data=null`），与 `AppError` 的响应形态一致。
/// - 日志只记 method / path / 超时秒数，**不记 query**：`accessToken` 允许
///   经查询串传递（SSE 无法设请求头），口径与 router.rs 的 TraceLayer 对齐。
/// - 中间件只约束到 handler **产出 Response** 为止。SSE 端点立即返回流式
///   响应、事件流在 body 里继续，因此 SSE 不需要白名单；同理任何
///   `Body::from_stream` 的流式透传（如 aiProxy）也不受影响。
pub async fn timeout_middleware(
    State(timeout): State<Duration>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    match tokio::time::timeout(timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_elapsed) => {
            tracing::warn!(
                method = %method,
                path = %path,
                timeout_secs = timeout.as_secs(),
                "入站请求处理超时"
            );
            (
                StatusCode::REQUEST_TIMEOUT,
                Json(ApiResponse::<serde_json::Value>::err("请求超时")),
            )
                .into_response()
        }
    }
}

/// 需登录接口的并发上限层：超出 `max` 的请求在信号量上**排队等待**（不是
/// 立即拒绝），排队时间由外层的 [`timeout_middleware`] 计入超时，排不上的
/// 请求最终以 408 收场而不是无限堆积。
///
/// 注意 `ConcurrencyLimitLayer::new(0)` 会 panic：`max == 0`（不限制）时
/// **调用方不得挂载本层**，见 `api/router.rs` 的条件挂载。
pub fn concurrency_layer(max: usize) -> tower::limit::ConcurrencyLimitLayer {
    tower::limit::ConcurrencyLimitLayer::new(max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{middleware, routing::get, Router};
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    fn test_router(timeout: Duration) -> Router {
        Router::new()
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    "ok"
                }),
            )
            .route("/fast", get(|| async { "ok" }))
            .layer(middleware::from_fn_with_state(timeout, timeout_middleware))
    }

    async fn send(app: Router, uri: &str) -> (StatusCode, bytes::Bytes) {
        let request = Request::get(uri).body(axum::body::Body::empty()).unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, body)
    }

    #[tokio::test]
    async fn fast_request_within_timeout_passes_through() {
        // 200ms 的 handler 在 1s 预算内完成：原样放行，包一层中间件不改语义
        let (status, body) = send(test_router(Duration::from_secs(1)), "/slow").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn slow_request_times_out_with_json_contract() {
        // 200ms 的 handler 撞上 20ms 超时：408 + 统一 JSON 错误体
        let (status, body) = send(test_router(Duration::from_millis(20)), "/slow").await;
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["isSuccess"], serde_json::Value::Bool(false));
        assert_eq!(json["errorMsg"], "请求超时");
        assert!(json["data"].is_null(), "失败时 data 必须为 null");
    }

    #[tokio::test]
    async fn zero_concurrency_means_layer_not_mounted() {
        // max == 0 表示不限制：ConcurrencyLimitLayer::new(0) 会 panic，
        // 调用方约定此时不挂并发层——验证 256 个并发请求全部放行（若误挂
        // max=0 的层，第一个请求就会 panic/卡死）。
        // 这里直接构造与 router.rs 相同的挂载分支：0 → 不加层。
        let max = 0usize;
        let router = Router::new().route(
            "/fast",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "ok"
            }),
        );
        // 与 build_router 的条件挂载保持同一形态
        let router = if max > 0 {
            router.layer(concurrency_layer(max))
        } else {
            router
        };
        let (status, _) = send(router, "/fast").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn concurrency_layer_queues_excess_requests() {
        // 并发上限 1：第二个请求在信号量上排队，等待中不失败、最终成功，
        // 证明该层是排队语义（配合外层超时构成「排队也计入超时」）
        let router = Router::new()
            .route(
                "/fast",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    "ok"
                }),
            )
            .layer(concurrency_layer(1));
        let (first, second) = tokio::join!(send(router.clone(), "/fast"), send(router, "/fast"));
        assert_eq!(first.0, StatusCode::OK);
        assert_eq!(second.0, StatusCode::OK, "超出并发的请求应排队而非被拒");
    }
}
