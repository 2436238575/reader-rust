//! `/v1` 图片管道：按 id 取图与任意地址封面解析。
//!
//! - `GET /v1/images/{imageId}?w=`：书源图片统一收口后的取图入口。响应是
//!   原始图片字节（不是 JSON），`Cache-Control: immutable`，`w` 按宽度
//!   派生缓存，`Accept` 含 `image/webp` 时输出有损 webp。
//! - `GET /v1/images:resolve?url=&w=`：兼容入口。`customCoverUrl` 等历史
//!   数据里可能是用户自设的任意 http(s) 地址，先登记成图片 id 再走同一条
//!   管道（转码、缓存、宽度派生一致）。
//!
//! legacy `/reader3/image/{id}` 与 `/reader3/cover` 继续保留；本模块只换
//! 入口与错误形态，图片本体/缓存/回源自愈完全复用 `ImageService`。

use crate::api::handlers::image::{accepts_webp, load_cover_variant, load_image_variant};
use crate::api::problem::{ErrorCode, FieldIssue, Problem};
use crate::api::v1::{V1Query, V1User};
use crate::api::AppState;
use crate::error::error::AppError;
use crate::service::image_service::ImageService;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use utoipa::IntoParams;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/images:resolve", get(resolve_image))
        .route("/v1/images/{imageId}", get(get_image))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImageVariantQuery {
    /// 目标像素宽：clamp 到 64..=2048，只降不升；省略表示原图宽度
    pub w: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ResolveImageQuery {
    /// 上游图片地址（仅 http/https；由服务端登记后走图片管道）
    pub url: Option<String>,
    /// 目标像素宽：clamp 到 64..=2048，只降不升；省略表示原图宽度
    pub w: Option<u32>,
}

fn image_problem(error: AppError) -> Problem {
    match error {
        AppError::NotFound(detail) => Problem::new(ErrorCode::ImageNotFound).detail(detail),
        AppError::Blocked(detail) => Problem::new(ErrorCode::UpstreamBlocked).detail(detail),
        other => Problem::from(other),
    }
}

fn field_error(pointer: &str, code: &str, detail: &str) -> Problem {
    Problem::new(ErrorCode::ValidationFailed)
        .detail(detail)
        .errors(vec![FieldIssue {
            pointer: pointer.to_string(),
            code: code.to_string(),
            detail: Some(detail.to_string()),
        }])
}

/// 按图片 id 取图。
#[utoipa::path(
    get,
    path = "/v1/images/{imageId}",
    tag = "images",
    operation_id = "getImage",
    params(
        ("imageId" = String, Path, description = "图片 id（32 位小写 md5 hex）"),
        ImageVariantQuery,
    ),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "图片字节（Content-Type 随原图或 Accept 协商格式；一年 immutable 缓存）", content_type = "application/octet-stream"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "图片不存在、上游抓取失败或地址已失效", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_image(
    State(state): State<AppState>,
    V1User(user): V1User,
    Path(image_id): Path<String>,
    V1Query(query): V1Query<ImageVariantQuery>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    // 入口显式校验：图片 id 会拼进缓存目录的文件路径，不能把 `../` 交给
    // 下游「先查库、查不到就 404」的顺序去兜。`ImageService::record` 里还有
    // 一层同样的闸，防止未来新增入口绕过 handler。
    if !ImageService::is_valid_image_id(&image_id) {
        return Err(Problem::new(ErrorCode::ImageNotFound).detail("图片不存在"));
    }
    load_image_variant(&state, &user.ns, &image_id, query.w, accepts_webp(&headers))
        .await
        .map_err(image_problem)
}

/// 把任意 http(s) 图片地址登记成 id 并返回图片字节（自定义封面兼容入口）。
#[utoipa::path(
    get,
    path = "/v1/images:resolve",
    tag = "images",
    operation_id = "resolveImage",
    params(ResolveImageQuery),
    security(("cookieAuth" = []), ("bearerAuth" = [])),
    responses(
        (status = 200, description = "图片字节（Content-Type 随原图或 Accept 协商格式；一年 immutable 缓存）", content_type = "application/octet-stream"),
        (status = 401, description = "未登录或令牌失效", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "图片抓取失败或上游地址已失效", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "url 缺失、为空或不是 http/https", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn resolve_image(
    State(state): State<AppState>,
    V1User(user): V1User,
    V1Query(query): V1Query<ResolveImageQuery>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let raw = query
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| field_error("/url", "REQUIRED", "url 不能为空"))?;
    let parsed =
        url::Url::parse(raw).map_err(|_| field_error("/url", "INVALID_URL", "url 不是合法地址"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(field_error("/url", "INVALID_URL", "url 只支持 http/https"));
    }
    match load_cover_variant(&state, &user.ns, raw, query.w, accepts_webp(&headers)).await {
        Ok(Some(response)) => Ok(response),
        Ok(None) => Err(field_error("/url", "INVALID_URL", "url 只支持 http/https")),
        Err(error) => Err(image_problem(error)),
    }
}
