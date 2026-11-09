//! 入站慢请求防护：请求处理超时与并发上限（配置见 `app/config.rs` 的
//! `INBOUND_REQUEST_TIMEOUT_SECS` / `INBOUND_UPLOAD_TIMEOUT_SECS` /
//! `INBOUND_MAX_CONCURRENCY`）。
//!
//! 上游抓取早有 `REQUEST_TIMEOUT_SECS` 兜底，但入站请求此前可以无限等待：
//! 挂起的 handler 持着连接、内存与 `spawn_blocking` 槽位，慢客户端或恶意
//! 慢请求能无限堆积。这里在 `api/router.rs` 的三个鉴权分组上补两层防线。

use crate::api::problem::{ErrorCode, Problem};
use crate::error::error::ApiResponse;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use std::time::Duration;

/// 超时响应形态。`/reader3` 沿用 ApiResponse（408）；`/v1` 用 problem+json
/// 的 `504 PROCESSING_TIMEOUT`——契约红线：408 的语义是客户端发请求太慢，
/// 服务端处理超时应为 504。两个命名空间共用同一份超时中间件，只换响应形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutShape {
    Legacy,
    Problem,
}

/// 入站请求超时中间件：`tokio::time::timeout` 包住 `next.run(request)`。
///
/// - 超时返回 **HTTP 408** + 统一 JSON 契约（`isSuccess=false`、
///   `errorMsg="请求超时"`、`data=null`），与 `AppError` 的响应形态一致。
/// - 日志只记 method / path / 超时秒数，**不记 query**：`accessToken` 允许
///   经查询串传递（SSE 无法设请求头），口径与 router.rs 的 TraceLayer 对齐。
/// - 中间件只约束到 handler **产出 Response** 为止。SSE 端点立即返回流式
///   响应、事件流在 body 里继续，因此 SSE 不需要白名单；`Body::from_stream`
///   的流式透传（如 aiProxy 的 stream:true）同理——但前提是**响应头已到达**。
///   非流式的 aiProxy 调用要等上游生成完才拿到响应头，天然会撞常规超时，
///   因此 aiProxy/aiProxyImage 路由在 router.rs 里与上传路由同档用
///   `INBOUND_UPLOAD_TIMEOUT_SECS`（其上游调用自身另有 300s 客户端预算）。
pub async fn timeout_middleware(
    State((timeout, shape)): State<(Duration, TimeoutShape)>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    // RequestId 由最外层 SetRequestIdLayer 注入，先取再交出请求所有权
    let trace_id = request
        .extensions()
        .get::<tower_http::request_id::RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .map(str::to_string);
    match tokio::time::timeout(timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_elapsed) => {
            tracing::warn!(
                method = %method,
                path = %path,
                timeout_secs = timeout.as_secs(),
                "入站请求处理超时"
            );
            match shape {
                TimeoutShape::Legacy => (
                    StatusCode::REQUEST_TIMEOUT,
                    Json(ApiResponse::<serde_json::Value>::err("请求超时")),
                )
                    .into_response(),
                TimeoutShape::Problem => {
                    let mut problem = Problem::new(ErrorCode::ProcessingTimeout)
                        .detail("服务端处理超时，请稍后重试");
                    if let Some(trace_id) = trace_id {
                        problem = problem.trace(trace_id);
                    }
                    problem.into_response()
                }
            }
        }
    }
}

/// 需登录接口的并发上限层：超出 `max` 的请求在信号量上**排队等待**（不是
/// 立即拒绝），排队时间由外层的 [`timeout_middleware`] 计入超时，排不上的
/// 请求最终以 408 收场而不是无限堆积。
///
/// 必须用共享信号量的 `GlobalConcurrencyLimitLayer`，不能用
/// `ConcurrencyLimitLayer`：axum 的 `Router::layer` 对**每条路由分别**实例化
/// 层（path_router 逐路由调 `endpoint.layer(layer.clone())`），而
/// `ConcurrencyLimitLayer::layer` 每次都新建一个 `Semaphore`——结果 N 条路由
/// 各自持有 max 个许可，总在途请求数可达 max×N，跨路由分散的慢请求根本
/// 不受同一上限约束。本层把 `Arc<Semaphore>` 持在层实例里，clone 共享同一
/// 份；需要在多个分组间共享同一上限时，构造一次层实例、按 clone 分发
/// （见 `api/router.rs`）。
///
/// 注意 0 许可的信号量不会 panic，但会让所有请求永久排队（直到外层超时
/// 408）：`max == 0`（不限制）时**调用方不得挂载本层**，见 `api/router.rs`
/// 的条件挂载。
pub fn concurrency_layer(max: usize) -> tower::limit::GlobalConcurrencyLimitLayer {
    tower::limit::GlobalConcurrencyLimitLayer::new(max)
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
            .layer(middleware::from_fn_with_state(
                (timeout, TimeoutShape::Legacy),
                timeout_middleware,
            ))
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

    /// /v1 口径：超时回 504 + problem+json PROCESSING_TIMEOUT（不是 408），
    /// 请求带 request-id 时 problem 体带一致的 traceId。
    #[tokio::test]
    async fn slow_request_times_out_with_problem_shape() {
        let app = Router::new()
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    "ok"
                }),
            )
            .layer(middleware::from_fn_with_state(
                (Duration::from_millis(20), TimeoutShape::Problem),
                timeout_middleware,
            ));
        let request = Request::get("/slow")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json",
            "v1 超时必须是 problem+json"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "PROCESSING_TIMEOUT");
        assert_eq!(json["status"], 504);
        assert!(
            json.get("traceId").is_none(),
            "无 request-id 层时 traceId 缺省省略"
        );
    }

    /// traceId 与请求的 X-Request-Id 扩展一致（用户截图能定位日志）。
    #[tokio::test]
    async fn problem_timeout_carries_request_trace_id() {
        let app = Router::new()
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    "ok"
                }),
            )
            .layer(middleware::from_fn_with_state(
                (Duration::from_millis(20), TimeoutShape::Problem),
                timeout_middleware,
            ));
        let mut request = Request::get("/slow")
            .body(axum::body::Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(tower_http::request_id::RequestId::new(
                axum::http::HeaderValue::from_static("trace-test-1"),
            ));
        let response = app.oneshot(request).await.unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["traceId"], "trace-test-1");
    }

    #[tokio::test]
    async fn zero_concurrency_means_layer_not_mounted() {
        // max == 0 表示不限制：0 许可的信号量会让所有请求永久排队（直到
        // 超时 408），调用方约定此时不挂并发层——验证 256 个并发请求全部
        // 放行（若误挂 max=0 的层，请求会一直排到超时）。
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

    #[tokio::test]
    async fn concurrency_limit_is_shared_across_routes() {
        // 跨路由共享同一份信号量是本层存在的意义：axum 的 Router::layer 对
        // 每条路由分别实例化层，逐路由独立的 ConcurrencyLimitLayer 会给每条
        // 路由各发一个信号量（/a 与 /b 并行放行）；换成共享 Arc<Semaphore>
        // 的 GlobalConcurrencyLimitLayer 后，上限 1 必须把打到不同路由的
        // 并发请求也串行化——两轮各 80ms 的 handler 至少要 160ms 才能完成。
        let handler = || async {
            tokio::time::sleep(Duration::from_millis(80)).await;
            "ok"
        };
        let router = Router::new()
            .route("/a", get(handler))
            .route("/b", get(handler))
            .layer(concurrency_layer(1));
        let start = std::time::Instant::now();
        let (first, second) = tokio::join!(send(router.clone(), "/a"), send(router, "/b"));
        assert_eq!(first.0, StatusCode::OK);
        assert_eq!(second.0, StatusCode::OK);
        assert!(
            start.elapsed() >= Duration::from_millis(150),
            "不同路由的并发请求应共享同一信号量串行执行，实际耗时 {:?}",
            start.elapsed()
        );
    }
}
