use crate::crawler::http_client::HttpClient;
use encoding_rs::Encoding;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::sleep;

/// 单次抓取的响应体上限（解压后的字节数）。
///
/// 抓取目标由用户导入的书源决定，可能返回超大响应或高度压缩的“解压炸弹”；
/// 不做限制时整包读入内存会直接把进程打爆。
pub const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

/// 边收边计数地读取响应体，超过 `limit` 立即中断。
pub async fn read_body_limited(
    mut res: reqwest::Response,
    limit: u64,
) -> anyhow::Result<bytes::Bytes> {
    if let Some(len) = res.content_length() {
        if len > limit {
            anyhow::bail!("响应体过大: {len} 字节（上限 {limit}）");
        }
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = res.chunk().await? {
        if buf.len() as u64 + chunk.len() as u64 > limit {
            anyhow::bail!("响应体超过上限 {limit} 字节");
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(bytes::Bytes::from(buf))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HttpMethod {
    GET,
    POST,
}

impl Default for HttpMethod {
    fn default() -> Self {
        Self::GET
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestSpec {
    pub url: String,
    pub method: HttpMethod,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub retry: usize,
    pub response_type: Option<String>,
    pub charset: Option<String>,
    pub proxy: Option<String>,
    pub server_id: Option<i64>,
    pub web_view: bool,
    pub web_js: Option<String>,
    pub web_view_delay_time: u64,
}

impl Default for RequestSpec {
    fn default() -> Self {
        Self {
            url: String::new(),
            method: HttpMethod::GET,
            headers: Vec::new(),
            body: None,
            retry: 2,
            response_type: None,
            charset: None,
            proxy: None,
            server_id: None,
            web_view: false,
            web_js: None,
            web_view_delay_time: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResponse {
    pub url: String,
    pub status: u16,
    pub body: String,
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
    pub is_successful: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct StrResponse {
    pub body: String,
    pub url: String,
    pub code: u16,
    pub headers: Vec<(String, String)>,
    pub is_successful: bool,
}

/// 抓取入口。
///
/// `user_ns` 决定使用哪个隔离的 HTTP 客户端（独立 Cookie jar），
/// 避免不同用户的站点会话相互串用。
pub async fn fetch(
    client: &HttpClient,
    user_ns: &str,
    req: RequestSpec,
) -> anyhow::Result<FetchResponse> {
    // 出站守卫：书源 URL 由用户导入，可能指向内网/云元数据地址
    if let Err(reason) =
        crate::crawler::url_guard::ensure_outbound_url_str_allowed(&req.url).await
    {
        anyhow::bail!("请求被出站策略拒绝: {reason}");
    }
    let http = client.client_for(user_ns)?;
    let mut last_err: Option<anyhow::Error> = None;
    let max_retries = req.retry;
    for attempt in 0..=max_retries {
        let req = req.clone();
        let mut builder = match req.method {
            HttpMethod::GET => http.get(&req.url),
            HttpMethod::POST => http.post(&req.url),
        };

        let mut has_content_type = false;
        for (k, v) in &req.headers {
            if k.to_lowercase() == "content-type" {
                has_content_type = true;
            }
            builder = builder.header(k, v);
        }

        if let Some(body) = req.body {
            if matches!(req.method, HttpMethod::POST) && !has_content_type {
                builder = builder.header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                );
            }
            // 注意：body 可能包含登录表单等敏感内容，绝不要打印
            builder = builder.body(body);
        }

        tracing::debug!(
            method = ?req.method,
            url = %req.url,
            "fetch 发起请求"
        );
        match builder.send().await {
            Ok(res) => {
                let status = res.status().as_u16();
                let is_successful = res.status().is_success();
                let url = res.url().to_string();
                tracing::debug!(status = status, url = %url, "fetch 收到响应");
                let content_type = res
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let headers = res
                    .headers()
                    .iter()
                    .filter_map(|(name, value)| {
                        value
                            .to_str()
                            .ok()
                            .map(|value| (name.to_string(), value.to_string()))
                    })
                    .collect::<Vec<_>>();
                let bytes = read_body_limited(res, MAX_RESPONSE_BYTES).await?;
                let mut body = if req
                    .response_type
                    .as_deref()
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false)
                {
                    hex::encode(&bytes)
                } else {
                    decode_body(&bytes, req.charset.as_deref(), content_type.as_deref())
                };
                if is_xml_response(content_type.as_deref(), &body)
                    && !body.trim_start().starts_with("<?xml")
                {
                    body = format!("<?xml version=\"1.0\"?>{}", body);
                }
                if status >= 500 && attempt < max_retries {
                    last_err = Some(anyhow::anyhow!("server error status {}", status));
                } else {
                    return Ok(FetchResponse {
                        url,
                        status,
                        body,
                        content_type,
                        headers,
                        is_successful,
                    });
                }
            }
            Err(e) => {
                last_err = Some(e.into());
            }
        }

        if attempt < max_retries {
            let backoff = 200u64 * (attempt as u64 + 1);
            sleep(Duration::from_millis(backoff)).await;
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("fetch failed")))
}

pub(crate) fn decode_body(bytes: &[u8], charset: Option<&str>, content_type: Option<&str>) -> String {
    let label = charset
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| charset_from_content_type(content_type))
        .or_else(|| charset_from_html_meta(bytes));

    if let Some(label) = label {
        if let Some(encoding) = Encoding::for_label(label.trim().as_bytes()) {
            let (text, _, _) = encoding.decode(bytes);
            return text.into_owned();
        }
    }

    String::from_utf8_lossy(bytes).into_owned()
}

fn charset_from_content_type(content_type: Option<&str>) -> Option<String> {
    content_type.and_then(|content_type| {
        content_type.split(';').find_map(|part| {
            let (key, value) = part.split_once('=')?;
            if key.trim().eq_ignore_ascii_case("charset") {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                (!value.is_empty()).then(|| value.to_string())
            } else {
                None
            }
        })
    })
}

fn charset_from_html_meta(bytes: &[u8]) -> Option<String> {
    let sniff_len = bytes.len().min(4096);
    let head = String::from_utf8_lossy(&bytes[..sniff_len]);
    let lower = head.to_ascii_lowercase();
    let index = lower.find("charset")?;
    let after = &head[index + "charset".len()..];
    let after = after.trim_start();
    let after = after.strip_prefix('=').unwrap_or(after).trim_start();
    let after = after
        .strip_prefix('"')
        .or_else(|| after.strip_prefix('\''))
        .unwrap_or(after);
    let label = after
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .collect::<String>();
    (!label.is_empty()).then_some(label)
}

impl From<FetchResponse> for StrResponse {
    fn from(value: FetchResponse) -> Self {
        Self {
            body: value.body,
            url: value.url,
            code: value.status,
            headers: value.headers,
            is_successful: value.is_successful,
        }
    }
}

impl From<StrResponse> for FetchResponse {
    fn from(value: StrResponse) -> Self {
        Self {
            url: value.url,
            status: value.code,
            body: value.body,
            content_type: None,
            headers: value.headers,
            is_successful: value.is_successful,
        }
    }
}

fn is_xml_response(content_type: Option<&str>, body: &str) -> bool {
    content_type
        .map(|value| value.to_ascii_lowercase().contains("xml"))
        .unwrap_or(false)
        || body.trim_start().starts_with("<rss")
        || body.trim_start().starts_with("<feed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_body_uses_response_charset() {
        let bytes = b"\xd0\xa1\xcb\xb5\xca\xd5\xb2\xd8\xc5\xc5\xd0\xd0\xb0\xf1";

        let text = decode_body(bytes, None, Some("text/html; charset=gb2312"));

        assert_eq!(text, "小说收藏排行榜");
    }

    #[test]
    fn decode_body_detects_html_meta_charset() {
        let bytes = b"<meta http-equiv=\"content-type\" content=\"text/html;charset=gb2312\"><title>\xb7\xc9\xc2\xac\xd0\xa1\xcb\xb5</title>";

        let text = decode_body(bytes, None, None);

        assert!(text.contains("飞卢小说"));
    }
}
