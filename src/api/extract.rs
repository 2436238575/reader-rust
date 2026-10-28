//! GET+POST 双注册端点的统一参数合并提取器。
//!
//! 全站唯一语义：**query 打底、body 字段级覆盖**（body 里的 `null` 表示抹掉
//! query 给的值）。JSON body 与 form-urlencoded body 走同一套规则。
//!
//! 此前各 handler 手写了至少四套合并（整体替换 / query 优先逐字段 /
//! `Option<Json>` 覆盖 / 文件级 merge_body），同一输入在不同端点结果相反——
//! 那是正确性问题。新端点一律用 `QueryOrBody<T>`，不要再手写合并。

use axum::extract::{FromRequest, Request};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::error::error::AppError;

/// 与 axum 默认请求体上限同口径（2MiB），参数合并不该吃掉整段大 body。
const MAX_MERGE_BODY_BYTES: usize = 2 * 1024 * 1024;

pub struct QueryOrBody<T>(pub T);

impl<S, T> FromRequest<S> for QueryOrBody<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request(req: Request, _state: &S) -> Result<Self, Self::Rejection> {
        let (parts, body) = req.into_parts();
        let mut merged: Map<String, Value> = Map::new();
        if let Some(query) = parts.uri.query() {
            for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
                merged.insert(k.into_owned(), Value::String(v.into_owned()));
            }
        }
        let bytes = axum::body::to_bytes(body, MAX_MERGE_BODY_BYTES)
            .await
            .map_err(|e| AppError::BadRequest(format!("请求体读取失败: {e}")))?;
        if !bytes.is_empty() {
            let content_type = parts
                .headers
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let is_json_ct =
                content_type.starts_with("application/json") || content_type.contains("+json");
            let is_form_ct = content_type.starts_with("application/x-www-form-urlencoded");

            let parsed_json = if is_form_ct {
                None
            } else {
                serde_json::from_slice::<Value>(&bytes).ok()
            };
            match parsed_json {
                Some(Value::Object(map)) => {
                    for (k, v) in map {
                        if v.is_null() {
                            merged.remove(&k);
                        } else {
                            merged.insert(k, v);
                        }
                    }
                }
                Some(_) => {
                    return Err(AppError::BadRequest(
                        "请求体必须是 JSON 对象或表单".to_string(),
                    ));
                }
                None => {
                    if is_json_ct {
                        // 声明了 JSON 却解析不了（典型是截断）：静默落回 query 会让
                        // 客户端以为 body 生效了——明确报 400 而不是给错误参数
                        return Err(AppError::BadRequest("JSON 请求体解析失败".to_string()));
                    }
                    // 无 content-type 的兜底：只接受「长得像表单」的 body（含 =），
                    // 其余（乱码/截断 JSON/二进制）一律 400 不猜
                    let text = std::str::from_utf8(&bytes).ok();
                    match text.filter(|t| t.contains('=')) {
                        Some(text) => {
                            for (k, v) in url::form_urlencoded::parse(text.as_bytes()) {
                                merged.insert(k.into_owned(), Value::String(v.into_owned()));
                            }
                        }
                        None => {
                            return Err(AppError::BadRequest(
                                "无法识别的请求体（既不是 JSON 也不是表单）".to_string(),
                            ));
                        }
                    }
                }
            }
        }
        // 终态统一走 form 语义解析：标量字符串自动强转数字/布尔，
        // 数组展开成重复键（serde_html_form 原生支持，axum Form 同库）。
        // 嵌套对象/数组没有表单对应物，必须 400 而不是抹成空串静默丢参数。
        let encoded = encode_merged(&merged)?;
        let value = serde_html_form::from_str::<T>(&encoded)
            .map_err(|e| AppError::BadRequest(format!("参数解析失败: {e}")))?;
        Ok(Self(value))
    }
}

/// 把合并后的参数表重新编码成 form-urlencoded 字符串。
///
/// **空值即缺省**：编码前丢弃所有空串值。serde_html_form 0.3 起空值语义反转
/// （`?a=` 对 `Option<String>` 从 `None` 变成 `Some("")`），不丢空值会让
/// 「显式传空串」被 handler 误判为「传了值」（典型：空 searchKey 触发空搜索，
/// 此前 `?key=` 得到的是 `None` → 400 "key required"）。丢弃空值后：
///
/// - `Option<T>` 字段：空串 → 键不存在 → `None`（与 0.2 完全一致）；
/// - 带 `#[serde(default)]` 的必填字段：空串 → 缺省 → 默认值（`""`/`0`），
///   与 0.2 下 `key=` 解析出的值相同；
/// - 无默认值的必填字段：空串 → 缺字段 400（0.2 下是空串放行、下游再报错，
///   结果都是拒绝，此处错误信息更准确）；
/// - JSON 数组里的 `null` 元素（`scalar_to_string` 映射为空串）一并跳过：
///   `{"ids":[1,null,3]}` → `[1,3]`，与对象里 `null` 表示抹除的口径一致。
fn encode_merged(merged: &Map<String, Value>) -> Result<String, AppError> {
    let mut pairs: Vec<(String, String)> = Vec::with_capacity(merged.len());
    for (k, v) in merged {
        match v {
            Value::Array(items) => {
                for item in items {
                    let s = scalar_to_string(item)?;
                    if !s.is_empty() {
                        pairs.push((k.clone(), s));
                    }
                }
            }
            other => {
                let s = scalar_to_string(other)?;
                if !s.is_empty() {
                    pairs.push((k.clone(), s));
                }
            }
        }
    }
    Ok(pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                url::form_urlencoded::byte_serialize(k.as_bytes()).collect::<String>(),
                url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("&"))
}

fn scalar_to_string(v: &Value) -> Result<String, AppError> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Null => Ok(String::new()),
        Value::Array(_) | Value::Object(_) => Err(AppError::BadRequest(
            "参数值不支持嵌套的 JSON 对象/数组".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Req {
        key: Option<String>,
        page: Option<i32>,
        ids: Option<Vec<i64>>,
        refresh: Option<bool>,
    }

    fn parse(query: &str, body: Option<&str>) -> Req {
        let mut merged: Map<String, Value> = Map::new();
        for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
            merged.insert(k.into_owned(), Value::String(v.into_owned()));
        }
        if let Some(body) = body {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(body) {
                for (k, v) in map {
                    if v.is_null() {
                        merged.remove(&k);
                    } else {
                        merged.insert(k, v);
                    }
                }
            } else {
                for (k, v) in url::form_urlencoded::parse(body.as_bytes()) {
                    merged.insert(k.into_owned(), Value::String(v.into_owned()));
                }
            }
        }
        // 与 from_request 共用同一编码器，空值归一逻辑不会两边漂移
        let encoded = encode_merged(&merged).expect("测试只喂扁平值");
        serde_html_form::from_str(&encoded).unwrap()
    }

    #[test]
    fn body_overrides_query_field_level() {
        // body 只覆盖它给了的字段，query 的其余字段保留
        let req = parse("key=三体&page=1&refresh=true", Some(r#"{"page":2}"#));
        assert_eq!(
            req,
            Req {
                key: Some("三体".into()),
                page: Some(2),
                ids: None,
                refresh: Some(true),
            }
        );
    }

    #[test]
    fn json_array_survives_roundtrip() {
        let req = parse("key=x", Some(r#"{"ids":[1,2,3]}"#));
        assert_eq!(req.ids, Some(vec![1, 2, 3]));
    }

    #[test]
    fn form_body_merges_and_coerces_scalars() {
        let req = parse("key=x", Some("page=3&refresh=false"));
        assert_eq!(req.page, Some(3));
        assert_eq!(req.refresh, Some(false));
    }

    #[test]
    fn body_null_erases_query_value() {
        let req = parse("key=x&page=1", Some(r#"{"page":null}"#));
        assert_eq!(req.key, Some("x".into()));
        assert_eq!(req.page, None);
    }

    #[test]
    fn empty_body_keeps_query() {
        let req = parse("key=x&page=7", None);
        assert_eq!(req.page, Some(7));
    }

    // ─── 空值归一：serde_html_form 0.3 起 `?a=` 对 Option<String> 会变成
    // Some("")，提取器在编码前丢弃空值，维持「空串即缺省」的 0.2 契约 ───

    #[test]
    fn empty_query_value_is_absent_for_option_string() {
        // `?key=` 必须等价于「没传 key」：否则空 searchKey 会绕过
        // handler 的 `ok_or("key required")` 触发一次空搜索
        let req = parse("key=&page=1", None);
        assert_eq!(req.key, None);
        assert_eq!(req.page, Some(1));
    }

    #[test]
    fn empty_json_string_is_absent_for_option_string() {
        // JSON 与 query 同一套语义：{"key":""} 同样按缺省处理
        let req = parse("page=2", Some(r#"{"key":""}"#));
        assert_eq!(req.key, None);
        assert_eq!(req.page, Some(2));
    }

    #[test]
    fn empty_form_body_value_is_absent() {
        let req = parse("", Some("key=&page=3"));
        assert_eq!(req.key, None);
        assert_eq!(req.page, Some(3));
    }

    #[test]
    fn empty_value_is_absent_for_option_seq() {
        // Option<Vec<T>>：0.3 起 `ids=` 会尝试把 "" 解析成 i64 而报错，
        // 归一后按缺省处理
        let req = parse("ids=&page=1", None);
        assert_eq!(req.ids, None);
        assert_eq!(req.page, Some(1));
    }

    #[test]
    fn trailing_empty_duplicate_key_still_means_absent() {
        // 重复键后者覆盖前者：key=x&key= 的终态是空串 → 缺省
        let req = parse("key=x&key=", None);
        assert_eq!(req.key, None);
    }

    #[test]
    fn null_array_items_are_skipped() {
        // 对象里的 null 表示抹除；数组里的 null 元素按同口径跳过
        let req = parse("", Some(r#"{"ids":[1,null,3]}"#));
        assert_eq!(req.ids, Some(vec![1, 3]));
    }

    // ─── 请求级用例：走真实 FromRequest（body 识别/拒绝语义） ───

    fn build_request(query: &str, body: &str, content_type: Option<&str>) -> Request {
        let mut builder = Request::builder().method("POST").uri(format!("/x?{query}"));
        if let Some(ct) = content_type {
            builder = builder.header(axum::http::header::CONTENT_TYPE, ct);
        }
        builder
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn malformed_json_with_json_content_type_is_rejected() {
        // 截断 JSON：静默落回 query 会让客户端以为 body 生效了
        let req = build_request("page=1", r#"{"page":2"#, Some("application/json"));
        let result = QueryOrBody::<Req>::from_request(req, &()).await;
        assert!(result.is_err(), "坏 JSON body 应 400 而不是静默落回 query");
    }

    #[tokio::test]
    async fn garbage_body_without_content_type_is_rejected() {
        let req = build_request("page=1", "garbage-without-equals", None);
        let result = QueryOrBody::<Req>::from_request(req, &()).await;
        assert!(result.is_err(), "无法识别的 body 应 400");
    }

    #[tokio::test]
    async fn json_body_without_content_type_still_works() {
        let req = build_request("page=1", r#"{"page":5}"#, None);
        let parsed = QueryOrBody::<Req>::from_request(req, &()).await.unwrap();
        assert_eq!(parsed.0.page, Some(5));
    }

    #[tokio::test]
    async fn empty_query_value_yields_none_for_option_string() {
        // 请求级用例：走真实 FromRequest，空串参数按缺省处理（见
        // encode_merged 的空值归一说明）
        let req = build_request("key=&page=1", "", None);
        let parsed = QueryOrBody::<Req>::from_request(req, &()).await.unwrap();
        assert_eq!(parsed.0.key, None);
        assert_eq!(parsed.0.page, Some(1));
    }

    #[tokio::test]
    async fn form_body_without_content_type_still_merges() {
        let req = build_request("key=x", "page=3&refresh=false", None);
        let parsed = QueryOrBody::<Req>::from_request(req, &()).await.unwrap();
        assert_eq!(parsed.0.page, Some(3));
        assert_eq!(parsed.0.key.as_deref(), Some("x"));
        assert_eq!(parsed.0.refresh, Some(false));
    }

    #[tokio::test]
    async fn json_array_body_is_rejected() {
        let req = build_request("page=1", r#"[1,2,3]"#, Some("application/json"));
        let result = QueryOrBody::<Req>::from_request(req, &()).await;
        assert!(result.is_err(), "非对象 JSON 无法做字段级合并，应 400");
    }

    #[tokio::test]
    async fn nested_object_value_is_rejected() {
        // {"a":{"b":1}} 过去被抹成 a= 静默丢参数，现在必须 400
        let req = build_request("page=1", r#"{"a":{"b":1}}"#, Some("application/json"));
        let result = QueryOrBody::<Req>::from_request(req, &()).await;
        assert!(
            matches!(result, Err(AppError::BadRequest(_))),
            "嵌套对象值应 400"
        );
    }

    #[tokio::test]
    async fn nested_value_inside_array_is_rejected() {
        // 数组里的对象/数组元素同样没有表单对应物
        let req = build_request("", r#"{"ids":[{"b":1}]}"#, Some("application/json"));
        let result = QueryOrBody::<Req>::from_request(req, &()).await;
        assert!(
            matches!(result, Err(AppError::BadRequest(_))),
            "数组内的嵌套元素应 400"
        );
        let req = build_request("", r#"{"ids":[[1,2]]}"#, Some("application/json"));
        let result = QueryOrBody::<Req>::from_request(req, &()).await;
        assert!(
            matches!(result, Err(AppError::BadRequest(_))),
            "数组内的嵌套数组应 400"
        );
    }
}
