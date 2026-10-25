use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

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
    /// 目标像素宽（64..=2048，只降不升）；缺省表示原图宽度。
    pub w: Option<u32>,
}

/// `/reader3/image/<id>` 的查询参数。
#[derive(Debug, Deserialize)]
pub struct ImageQuery {
    /// 目标像素宽（64..=2048，只降不升）；缺省表示原图宽度。
    pub w: Option<u32>,
}

/// 图片 id 只能是 md5 hex：axum 会把 `%2F` 解码进路径段，
/// 不校验就把 `../` 拼进了缓存目录的文件路径。
fn is_valid_image_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Accept 头里是否明确列了 `image/webp`。
///
/// `*/*` 不算：没开口就不冒进，免得把 webp 硬塞给不认它的客户端。
fn accepts_webp(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(',')
                .filter_map(|item| item.split(';').next())
                .any(|mime| mime.trim().eq_ignore_ascii_case("image/webp"))
        })
        .unwrap_or(false)
}

/// 本站取图：`/reader3/image/<id>`。
///
/// 缓存命中直接返回；没命中就抓上游（HEIC 转 JPEG）后落盘，之后长期命中。
/// 上游地址过期（签名失效）时，用登记时留下的书籍上下文回源刷新一次再试。
///
/// 查询参数 `w`（可选）：按宽度缩小（只降不升）；请求头 `Accept` 含
/// `image/webp` 时输出有损 webp，否则 JPEG——内容随 Accept 变化，响应带
/// `Vary: Accept`。
pub async fn get_image(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<ImageQuery>,
    headers: HeaderMap,
) -> Result<Response<Body>, AppError> {
    if !is_valid_image_id(&id) {
        return Err(AppError::NotFound("图片不存在".to_string()));
    }
    let user_ns = user.0.ns.clone();
    let accept_webp = accepts_webp(&headers);
    match state
        .image_service
        .load_variant(&id, q.w, accept_webp)
        .await
    {
        Ok((bytes, content_type)) => Ok(image_response(bytes, content_type)),
        Err(err) => match heal_source_url(&state, &user_ns, &id, q.w, accept_webp).await {
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
    headers: HeaderMap,
) -> Result<Response<Body>, AppError> {
    let user_ns = user.0.ns.clone();
    let Some(url) = q.path.as_deref().map(str::trim).filter(|u| !u.is_empty()) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let accept_webp = accepts_webp(&headers);
    let Some(id) = state.image_service.register("cover", url, None).await else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    match state
        .image_service
        .load_variant(&id, q.w, accept_webp)
        .await
    {
        Ok((bytes, content_type)) => Ok(image_response(bytes, content_type)),
        Err(err) => {
            // 兼容入口（customCoverUrl）没有书籍上下文，无法回源刷新
            let _ = heal_source_url(&state, &user_ns, &id, q.w, accept_webp).await;
            let _ = err;
            Ok(StatusCode::NOT_FOUND.into_response())
        }
    }
}

/// 上游地址失效时回源刷新一次：按记录里的 bookId 解析当前绑定（换源后
/// 也能拿到当前书源），重新求值书源的封面地址。刷新成功后**重新走
/// `load_variant`**——自愈不能把未缩放的原图漏给要派生图的请求。
async fn heal_source_url(
    state: &AppState,
    user_ns: &str,
    id: &str,
    width: Option<u32>,
    accept_webp: bool,
) -> Option<(Vec<u8>, String)> {
    let record = state.image_service.record(id).await?;
    let book_id = record.book_id.clone()?;
    let row = state
        .book_service
        .book_repo()
        .get(user_ns, &book_id)
        .await
        .ok()??;
    let source_id = row.book_source_id?;
    let source = state
        .book_source_service
        .get_by_id(user_ns, source_id)
        .await
        .ok()??;
    // 书籍详情有缓存，这里必须绕过，否则拿回来的还是那条过期地址
    let info = state
        .book_service
        .get_book_info(user_ns, &book_id, &source, &row.book_url, true)
        .await
        .ok()?;
    let fresh = info.cover_url.filter(|url| !url.trim().is_empty())?;
    state.image_service.update_source_url(id, &fresh).await;
    match state
        .image_service
        .load_variant(id, width, accept_webp)
        .await
    {
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
    // 同一 URL 的表示随 Accept 协商变化，缓存必须按 Accept 区分
    headers.insert(header::VARY, HeaderValue::from_static("Accept"));
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

#[cfg(test)]
mod tests {
    use super::{accepts_webp, is_valid_image_id};
    use axum::http::{header, HeaderMap, HeaderValue};

    #[test]
    fn image_id_must_be_lowercase_md5_hex() {
        assert!(is_valid_image_id("0123456789abcdef0123456789abcdef"));
        // 长度不对、大写、非 hex、路径穿越全部拒绝
        assert!(!is_valid_image_id("abc"));
        assert!(!is_valid_image_id("0123456789ABCDEF0123456789ABCDEF"));
        assert!(!is_valid_image_id("0123456789abcdef0123456789abcdeg"));
        assert!(!is_valid_image_id("../../etc/passwd"));
        assert!(!is_valid_image_id("..%2F..%2Fetc"));
        assert!(!is_valid_image_id(""));
    }

    #[test]
    fn accept_header_webp_negotiation() {
        let mut headers = HeaderMap::new();
        assert!(!accepts_webp(&headers), "无 Accept 头不算支持 webp");

        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static(
                "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            ),
        );
        assert!(accepts_webp(&headers), "浏览器常见 Accept 应命中");

        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9"),
        );
        assert!(!accepts_webp(&headers));

        headers.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        assert!(!accepts_webp(&headers), "*/* 不算明确支持 webp");

        // 带权重的写法也要认得
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("image/jpeg;q=0.9,IMAGE/WEBP;q=0.8"),
        );
        assert!(accepts_webp(&headers), "大小写与 q 权重都不影响判定");
    }
}
