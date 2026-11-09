//! ETag 与条件请求的共用实现。
//!
//! 宽松比较规则（RFC 9110 §13.1.2：GET/HEAD 用弱比较）错一处就是一个
//! 「缓存永远不命中」或「永不失效」的 bug——全站只允许这一份实现。
//! `strong_etag` 对响应的精确字节取 md5：同一份数据字节级稳定，字段确定性。

use axum::http::HeaderValue;
use md5::{Digest, Md5};

/// 对响应字节的强 ETag：`"<md5-hex>"`。
pub(crate) fn strong_etag(bytes: &[u8]) -> HeaderValue {
    let digest = Md5::digest(bytes);
    HeaderValue::from_str(&format!("\"{}\"", hex::encode(digest)))
        .expect("md5 hex + 引号必为合法 header 值")
}

/// `If-None-Match` 的宽松比较：候选按逗号分隔，忽略 `W/` 弱前缀与两端
/// 空白；`*` 匹配任意现有表示。头缺失或非法 UTF-8 一律不匹配——
/// 宁可多回一次 200，不误报 304。
pub(crate) fn if_none_match_matches(
    if_none_match: Option<&HeaderValue>,
    etag: &HeaderValue,
) -> bool {
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
        candidate
            .trim()
            .strip_prefix("W/")
            .unwrap_or(candidate.trim())
            == current
    })
}
