//! 章节列表端点与目录响应的 ETag / `If-None-Match`（304）处理。

use super::resolve_book;
use crate::api::extract::QueryOrBody;
use crate::api::AppState;
use crate::auth::CurrentUser;
use crate::error::error::{ApiResponse, AppError};
use crate::service::local_epub_book::is_local_epub_url;
use crate::service::local_txt_book::is_local_txt_url;
use crate::util::text::repair_encoded_url;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{extract::State, Json};
use md5::{Digest, Md5};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct ChapterListRequest {
    #[serde(rename = "bookId")]
    pub book_id: Option<String>,
    pub refresh: Option<i32>,
}

/// 目录响应的强 ETag：对实际返回的章节列表序列化后取 md5。
/// 同一份目录字节级稳定 → ETag 稳定；章节增删或标题/URL 变化必然改变
/// 序列化字节 → ETag 变化，客户端据此复用浏览器缓存（304）。
fn chapter_list_etag(chapters: &Value) -> HeaderValue {
    let bytes = serde_json::to_vec(chapters).unwrap_or_default();
    let digest = Md5::digest(&bytes);
    let quoted = format!("\"{}\"", hex::encode(digest));
    HeaderValue::from_str(&quoted).expect("md5 hex + 引号必为合法 header 值")
}

/// `If-None-Match` 的宽松比较（RFC 9110 §13.1.2：GET/HEAD 用弱比较）：
/// 候选按逗号分隔，忽略 `W/` 弱前缀与两端空白；`*` 匹配任意现有表示。
/// 头缺失或非法 UTF-8 一律不匹配——宁可多回一次 200，不误报 304。
fn if_none_match_matches(if_none_match: Option<&HeaderValue>, etag: &HeaderValue) -> bool {
    let Some(header_value) = if_none_match else {
        return false;
    };
    let Ok(raw) = header_value.to_str() else {
        return false;
    };
    let raw = raw.trim();
    if raw == "*" {
        return true;
    }
    let Some(current) = etag.to_str().ok() else {
        return false;
    };
    raw.split(',').any(|candidate| {
        let candidate = candidate.trim();
        let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
        candidate == current
    })
}

/// 目录接口统一出口：未变化时 304 空 body，否则 200 JSON；
/// 两种结果都带 ETag 与 `Cache-Control: private, no-cache`
/// （只允许当前用户的私有缓存，且每次使用前必须带 If-None-Match 再验证）。
fn chapter_list_response(request_headers: &HeaderMap, chapters: Value) -> Response {
    let etag = chapter_list_etag(&chapters);
    let not_modified = if_none_match_matches(request_headers.get(header::IF_NONE_MATCH), &etag);
    let mut response = if not_modified {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Json(ApiResponse::ok(chapters)).into_response()
    };
    response.headers_mut().insert(header::ETAG, etag);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    response
}

pub async fn get_chapter_list(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    QueryOrBody(req): QueryOrBody<ChapterListRequest>,
) -> Result<Response, AppError> {
    let user_ns = user.0.ns.clone();

    let book_id = req
        .book_id
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("bookId required".to_string()))?;
    let do_refresh = req.refresh.unwrap_or(0) > 0;
    let resolved = resolve_book(&state, &user_ns, &book_id).await?;

    if is_local_txt_url(&resolved.book_url) {
        let chapters = state
            .local_txt_book_service
            .get_chapter_list(&user_ns, &resolved.book_url)
            .await?;
        return Ok(chapter_list_response(
            &headers,
            serde_json::to_value(chapters).unwrap_or_default(),
        ));
    }
    if is_local_epub_url(&resolved.book_url) {
        let chapters = state
            .local_epub_book_service
            .get_chapter_list(&user_ns, &resolved.book_url)
            .await?;
        return Ok(chapter_list_response(
            &headers,
            serde_json::to_value(chapters).unwrap_or_default(),
        ));
    }

    // 目录地址：登记表 > 书籍详情（详情缓存按 bookId，顺带回填 toc_url）> book_url
    let toc_url = match resolved.toc_url.filter(|u| !u.trim().is_empty()) {
        Some(toc_url) => toc_url,
        None => state
            .book_service
            .get_book_info(
                &user_ns,
                &book_id,
                &resolved.source,
                &resolved.book_url,
                false,
            )
            .await?
            .toc_url
            .unwrap_or_else(|| resolved.book_url.clone()),
    };
    let toc_url = repair_encoded_url(&toc_url);

    if do_refresh {
        let _ = state
            .book_service
            .delete_chapter_list_cache(&user_ns, &book_id)
            .await;
    } else if let Ok(Some(cached)) = state
        .book_service
        .load_fresh_chapter_list_cache(&user_ns, &book_id)
        .await
    {
        if !cached.is_empty() {
            return Ok(chapter_list_response(
                &headers,
                serde_json::to_value(cached).unwrap_or_default(),
            ));
        }
    }

    // Get first page of chapters
    let (chapters, pagination) = state
        .book_service
        .get_chapter_list_first_page(&user_ns, &resolved.source, &toc_url)
        .await?;

    // Save first page to cache immediately.
    // 还有待补全的分页时先标记未完成（读侧只给 60s 短 TTL），后台补全成功
    // 由 append 把 meta 置为完成；单页目录本身就是完整目录，直接按完成落盘。
    if pagination.pending_urls.is_empty() {
        let _ = state
            .book_service
            .save_chapter_list_cache(&user_ns, &book_id, &chapters)
            .await;
    } else {
        let _ = state
            .book_service
            .save_chapter_list_cache_incomplete(&user_ns, &book_id, &chapters)
            .await;
    }

    // If there are more pages to fetch, do it in background.
    // 同一本书的补全任务并发去重：双击/刷新在首屏落盘前竞态时，
    // 两个任务会各自抓全部分页并互相覆盖，缓存里出现重复章节。
    let toc_guard = if !pagination.pending_urls.is_empty() {
        state.book_service.try_begin_toc_fill(&user_ns, &book_id)
    } else {
        None
    };
    if let Some(toc_guard) = toc_guard {
        let state_clone = state.clone();
        let user_ns_clone = user_ns.clone();
        let book_id_clone = book_id.clone();

        tokio::spawn(async move {
            // RAII：任务结束/panic 都自动释放补全标志位
            let _toc_guard = toc_guard;
            tracing::debug!("starting background chapter fetch");
            // complete=false 表示分页被截断（MAX_TOC_PAGES 上限 / 整页重复熔断）：
            // 已抓到的章节照常并入缓存，但 meta 保持未完成，读侧 60s 后可重试
            let remaining = match state_clone
                .book_service
                .fetch_remaining_chapters(pagination)
                .await
            {
                Ok((remaining, complete)) if !remaining.is_empty() => Some((remaining, complete)),
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!("background chapter fetch failed: {:?}", e);
                    None
                }
            };
            if let Some((remaining, complete)) = remaining {
                match state_clone
                    .book_service
                    .append_chapter_list_cache_with_complete(
                        &user_ns_clone,
                        &book_id_clone,
                        &remaining,
                        complete,
                    )
                    .await
                {
                    Ok(all_chapters) => {
                        tracing::debug!(
                            "background chapter fetch complete: {}",
                            all_chapters.len()
                        );
                    }
                    Err(e) => {
                        tracing::warn!("failed to append background chapters to cache: {:?}", e);
                    }
                }
            }
        });
    }

    Ok(chapter_list_response(
        &headers,
        serde_json::to_value(chapters).unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{chapter_list_etag, chapter_list_response, if_none_match_matches};
    use axum::http::{header, HeaderMap, HeaderValue};
    use serde_json::json;

    fn sample_toc(tag: &str) -> serde_json::Value {
        json!([
            { "title": format!("第一章 {tag}"), "url": "https://example.com/c1", "index": 0 },
            { "title": "第二章", "url": "https://example.com/c2", "index": 1 },
        ])
    }

    #[test]
    fn chapter_list_etag_is_stable_quoted_md5_and_tracks_content() {
        let etag = chapter_list_etag(&sample_toc("x"));
        let quoted = etag.to_str().unwrap();
        // 强验证器格式：两侧引号 + 32 位小写 hex
        assert_eq!(quoted.len(), 34);
        assert!(quoted.starts_with('"') && quoted.ends_with('"'));
        assert!(
            quoted
                .trim_matches('"')
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "ETag 应是 md5 hex: {quoted}"
        );
        // 同一内容字节级稳定
        assert_eq!(
            chapter_list_etag(&sample_toc("x")).to_str().unwrap(),
            quoted
        );
        // 任一章节变化（标题）→ ETag 变化
        assert_ne!(
            chapter_list_etag(&sample_toc("y")).to_str().unwrap(),
            quoted
        );
        // 章节数量变化 → ETag 变化
        assert_ne!(
            chapter_list_etag(
                &json!([{ "title": "第一章 x", "url": "https://example.com/c1", "index": 0 }])
            )
            .to_str()
            .unwrap(),
            quoted
        );
    }

    #[test]
    fn if_none_match_matches_exact_weak_list_and_star() {
        let etag = chapter_list_etag(&sample_toc("x"));

        // 头缺失 → 不匹配（回 200）
        assert!(!if_none_match_matches(None, &etag));
        // 精确匹配
        let header = |value: &str| HeaderValue::from_str(value).unwrap();
        assert!(if_none_match_matches(
            Some(&header(etag.to_str().unwrap())),
            &etag
        ));
        // W/ 弱前缀（RFC 9110 对 If-None-Match 用弱比较）
        let weak = format!("W/{}", etag.to_str().unwrap());
        assert!(if_none_match_matches(Some(&header(&weak)), &etag));
        // 逗号列表 + 空白 + 其他候选
        let list = format!("\"stale\" , {}", weak);
        assert!(if_none_match_matches(Some(&header(&list)), &etag));
        // `*` 匹配任意现有表示
        assert!(if_none_match_matches(Some(&header("*")), &etag));
        // 不同 ETag → 不匹配
        assert!(!if_none_match_matches(
            Some(&header("\"00000000000000000000000000000000\"")),
            &etag
        ));
        // 候选缺引号（格式错误）→ 不匹配，宁回 200 不误 304
        let unquoted = etag.to_str().unwrap().trim_matches('"').to_string();
        assert!(!if_none_match_matches(Some(&header(&unquoted)), &etag));
    }

    #[tokio::test]
    async fn chapter_list_response_switches_on_if_none_match() {
        let toc = sample_toc("x");
        let etag = chapter_list_etag(&toc).to_str().unwrap().to_string();

        // 无条件 GET：200 + JSON 包装 + 两个头
        let response = chapter_list_response(&HeaderMap::new(), toc.clone());
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(response.headers().get(header::ETAG).unwrap(), etag.as_str());
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-cache"
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["isSuccess"], json!(true));
        assert_eq!(payload["data"], toc);

        // 命中 If-None-Match：304 + 空 body + 头照带
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, HeaderValue::from_str(&etag).unwrap());
        let response = chapter_list_response(&headers, toc);
        assert_eq!(response.status(), axum::http::StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers().get(header::ETAG).unwrap(), etag.as_str());
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-cache"
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(body.is_empty(), "304 body 必须为空");
    }
}
