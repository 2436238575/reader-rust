use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::api::handlers::book::resolve_book_source;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::AppError;

/// 老接口 `/reader3/cover?path=<封面地址>` 的入参。
///
/// 仍然保留：书架上早已存下的是书源地址，前端第一次加载时会走这里。
/// 内部转成「登记 → 取图」，与 `/reader3/image/<id>` 完全同一条管道。
#[derive(Debug, Deserialize)]
pub struct CoverQuery {
    pub path: Option<String>,
}

/// 本站取图：`/reader3/image/<id>`。
///
/// 缓存命中直接返回；没命中就抓上游（HEIC 转 JPEG）后落盘，之后长期命中。
/// 上游地址过期（签名失效）时，用登记时留下的书籍上下文回源刷新一次再试。
pub async fn get_image(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response<Body>, AppError> {
    let user_ns = user.0.ns.clone();
    match state.image_service.load(&id).await {
        Ok((bytes, content_type)) => Ok(image_response(bytes, content_type)),
        Err(err) => match heal_source_url(&state, &user_ns, &id).await {
            Some((bytes, content_type)) => Ok(image_response(bytes, content_type)),
            None => Err(err),
        },
    }
}

/// 封面（兼容入口）：把书源地址登记成 id 再取图。
pub async fn get_book_cover(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(q): Query<CoverQuery>,
) -> Result<Response<Body>, AppError> {
    let user_ns = user.0.ns.clone();
    let Some(url) = q.path.as_deref().map(str::trim).filter(|u| !u.is_empty()) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let Some(id) = state.image_service.register("cover", url, None, None).await else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    match state.image_service.load(&id).await {
        Ok((bytes, content_type)) => Ok(image_response(bytes, content_type)),
        Err(err) => {
            // 老入口没有书籍上下文，回源刷新只能靠书架反查
            match heal_source_url(&state, &user_ns, &id).await {
                Some((bytes, content_type)) => Ok(image_response(bytes, content_type)),
                None => {
                    tracing::warn!("封面抓取失败 url={url} err={err:?}");
                    Ok(StatusCode::NOT_FOUND.into_response())
                }
            }
        }
    }
}

/// 上游地址失效时回源刷新一次：拿登记时的书籍上下文重新求值书源的封面地址。
async fn heal_source_url(state: &AppState, user_ns: &str, id: &str) -> Option<(Vec<u8>, String)> {
    let record = state.image_service.record(id).await?;
    let book_url = record.book_url.clone()?;
    let book_source_url = record.book_source_url.clone();
    let source = resolve_book_source(state, user_ns, book_source_url, None, Some(&book_url))
        .await
        .ok()?;
    // 书籍详情有 10 分钟缓存，这里必须绕过，否则拿回来的还是那条过期地址
    let info = state
        .book_service
        .get_book_info(user_ns, &source, &book_url, true)
        .await
        .ok()?;
    let fresh = info.cover_url.filter(|url| !url.trim().is_empty())?;
    state.image_service.update_source_url(id, &fresh).await;
    match state.image_service.load(id).await {
        Ok(pair) => {
            tracing::info!("图片地址已回源刷新 id={id}");
            Some(pair)
        }
        Err(err) => {
            tracing::warn!("回源刷新后仍取不到图 id={id} err={err:?}");
            None
        }
    }
}

fn image_response(bytes: Vec<u8>, content_type: String) -> Response<Body> {
    let mut resp = Response::new(Body::from(bytes));
    let headers = resp.headers_mut();
    // id 稳定、内容不变，可以放心长缓存
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    resp
}

/// 包一层响应，并把书籍封面换成本站取图地址。
///
/// 前端因此不需要知道书源地址（更不需要知道它什么时候过期）；没有改写到的地方
/// 仍然可以走兼容入口 `/reader3/cover?path=`，所以这里只做「能改就改」。
pub(crate) async fn ok_with_cover_routes(
    state: &AppState,
    value: impl serde::Serialize,
) -> Json<crate::error::error::ApiResponse<serde_json::Value>> {
    let mut value = serde_json::to_value(value).unwrap_or_default();
    state.image_service.rewrite_cover_urls(&mut value).await;
    Json(crate::error::error::ApiResponse::ok(value))
}
