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

#[axum::async_trait]
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
            let mut is_json = false;
            if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&bytes) {
                is_json = true;
                for (k, v) in map {
                    if v.is_null() {
                        merged.remove(&k);
                    } else {
                        merged.insert(k, v);
                    }
                }
            }
            if !is_json {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    for (k, v) in url::form_urlencoded::parse(text.as_bytes()) {
                        merged.insert(k.into_owned(), Value::String(v.into_owned()));
                    }
                }
            }
        }
        // 终态统一走 form 语义解析：标量字符串自动强转数字/布尔，
        // 数组展开成重复键（serde_html_form 原生支持，axum Form 同库）。
        let encoded = merged
            .iter()
            .flat_map(|(k, v)| match v {
                Value::Array(items) => items
                    .iter()
                    .map(|item| (k.clone(), scalar_to_string(item)))
                    .collect::<Vec<_>>(),
                other => vec![(k.clone(), scalar_to_string(other))],
            })
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    url::form_urlencoded::byte_serialize(k.as_bytes()).collect::<String>(),
                    url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        let value = serde_html_form::from_str::<T>(&encoded)
            .map_err(|e| AppError::BadRequest(format!("参数解析失败: {e}")))?;
        Ok(Self(value))
    }
}

fn scalar_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
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
        let encoded = merged
            .iter()
            .flat_map(|(k, v)| match v {
                Value::Array(items) => items
                    .iter()
                    .map(|item| (k.clone(), scalar_to_string(item)))
                    .collect::<Vec<_>>(),
                other => vec![(k.clone(), scalar_to_string(other))],
            })
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    url::form_urlencoded::byte_serialize(k.as_bytes()).collect::<String>(),
                    url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("&");
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
}
