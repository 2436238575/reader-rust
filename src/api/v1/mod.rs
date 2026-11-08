//! `/v1` 命名空间：RESTful 契约接口（Draft-2）。
//!
//! 与 `/reader3` 按域切换并存（见 goal 决策 D3）：每个域的 v1 端点落在
//! `v1/` 下的子模块，复用现有 service 层；响应一律具名 schema + problem+json
//! 错误，`ApiResponse` 不进入本命名空间。鉴权中间件独立于 `/reader3` 的
//! `require_auth`——同样的令牌校验逻辑，失败返回 problem+json（401 +
//! `AuthExpired`）而不是 `ApiResponse`。
//!
//! OpenAPI 文档由代码生成：`openapi.yaml()` 的输出与仓库根目录
//! `openapi.yaml` 做漂移检查（`tests/openapi_drift.rs`），不一致即失败；
//! 更新锚文件用 `UPDATE_OPENAPI=1 cargo test --test openapi_drift`。

use crate::api::problem::{ErrorCode, Problem};
use crate::api::AppState;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use axum::Router;
use utoipa::OpenApi;

mod me;

/// `/v1` 路由：每个子模块自带 router，这里汇总并统一挂鉴权。
/// 非法/缺失令牌返回 problem+json，与 `/reader3` 的 `ApiResponse` 形态隔离。
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(me::router())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth,
        ))
        .with_state(state)
}

/// 未注册的 `/v1` 路径：problem+json 404（`/v1` 是纯 API 命名空间，
/// 不落到静态文件服务）。
pub async fn not_found() -> Problem {
    Problem::new(ErrorCode::NotFound).detail("接口不存在")
}

/// `/v1` 的强制鉴权：逻辑与 `auth::middleware::require_auth` 一致
/// （同一套令牌校验与撤销检查），失败形态换成 problem+json。
/// CSRF 头检查与 Cookie 语义调整随 auth 域切换落地，当前只挂 `/v1`。
async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, Problem> {
    use crate::auth::middleware::{authenticate, extract_token};
    let unauthorized = || {
        let mut problem = Problem::new(ErrorCode::AuthExpired).detail("未登录或令牌已过期");
        if let Some(request_id) = request
            .extensions()
            .get::<tower_http::request_id::RequestId>()
        {
            problem = problem.trace(request_id.header_value().to_str().unwrap_or("unknown"));
        }
        problem
    };
    let token = extract_token(&request).ok_or_else(unauthorized)?;
    let user = authenticate(&state.auth, token)
        .await
        .map_err(Problem::from)
        .map_err(|problem| attach_trace(problem, &request))?;
    // 限定用途令牌（书源登录代理）只服务 /reader3 代理路径，不进 /v1
    if user.scope.is_some() {
        return Err(attach_trace(
            Problem::new(ErrorCode::ScopeForbidden).detail("限定令牌不能访问 /v1 接口"),
            &request,
        ));
    }
    request.extensions_mut().insert(user);
    Ok(next.run(request).await)
}

/// problem 体带 traceId，与响应头 `X-Request-Id` 一致（用户截图能定位日志）。
fn attach_trace(mut problem: Problem, request: &Request) -> Problem {
    if problem.trace_id.is_none() {
        if let Some(request_id) = request
            .extensions()
            .get::<tower_http::request_id::RequestId>()
        {
            problem = problem.trace(request_id.header_value().to_str().unwrap_or("unknown"));
        }
    }
    problem
}

/// 由代码生成的 OpenAPI 文档（YAML）。`tests/openapi_drift.rs` 用它与
/// 仓库根目录的 `openapi.yaml` 做一致性检查。
pub fn openapi_yaml() -> String {
    V1ApiDoc::openapi()
        .to_yaml()
        .expect("openapi yaml serialization is infallible for static schemas")
}

#[derive(utoipa::OpenApi)]
#[openapi(
    info(title = "reader-rust v1 API", version = "0.1.0"),
    paths(me::get_me),
    components(schemas(me::Me, Problem, ErrorCode, crate::api::problem::FieldIssue)),
    tags((name = "me", description = "当前账号"))
)]
struct V1ApiDoc;
