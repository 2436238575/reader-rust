use crate::util::hash::md5_hex;
use crate::util::text::{apply_regex_replace, strip_whitespace};
use aes::Aes128;
use base64::Engine;
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use chrono::{Local, TimeZone};
use once_cell::sync::Lazy;
use reqwest::blocking::Client;
use reqwest::Method;
use rquickjs::function::Func;
use rquickjs::{Context, Object, Runtime, Value};
use serde_json::Value as JsonValue;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// 书源 JS 的全局 KV（`cache.put/get`、`kv_put/get`）。
///
/// 键一律经过 [`scoped_key`] 加用户前缀，避免不同书源/不同用户的键互相覆盖
/// 或读取到他人写入的值。
static JS_KV: Lazy<Mutex<HashMap<String, String>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static JS_LIB_CACHE: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
/// 书源 JS 的运行时资源上限。
///
/// 书源内容完全由第三方提供，而 QuickJS 求值是同步的；不设边界时一条
/// `while(true){}` 或一次正则灾难性回溯就能永久占住一个 worker 线程。
const JS_MEMORY_LIMIT_BYTES: usize = 128 * 1024 * 1024;
const JS_MAX_STACK_BYTES: usize = 1024 * 1024;
/// 单次求值的最长时间；由中断处理器打断并抛出不可捕获异常。
const JS_EVAL_TIMEOUT: Duration = Duration::from_secs(5);
/// 单次求值允许返回的最大字符串长度。
const JS_MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;
/// `java.ajax` 等内部请求的超时。
const JS_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// 书源 JS 内 `java.ajax/get/post/put` 使用的客户端池。
///
/// 同样按用户命名空间隔离：否则 A 用户在站点登录得到的会话 Cookie
/// 会被 B 用户书源的 `java.ajax` 自动带上。
static JS_HTTP_CLIENTS: Lazy<Mutex<HashMap<String, Client>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
/// 未显式指定命名空间时的归属（封面等匿名请求同此）。
const DEFAULT_USER_NS: &str = "public";
type Aes128CbcDecryptor = cbc::Decryptor<Aes128>;
thread_local! {
    static ACTIVE_JS_LIB: RefCell<Option<String>> = const { RefCell::new(None) };
    /// 当前求值所属的用户命名空间。
    static ACTIVE_USER_NS: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub fn with_js_lib<T>(js_lib: Option<&str>, f: impl FnOnce() -> T) -> T {
    ACTIVE_JS_LIB.with(|cell| {
        let previous = cell.replace(js_lib.map(|value| value.to_string()));
        let result = f();
        cell.replace(previous);
        result
    })
}

/// 在指定用户命名空间下执行闭包。
///
/// 书源 JS 的 `cache`/`kv`/`java.*` 都会读取当前命名空间做隔离；
/// 未设置时归入 [`DEFAULT_USER_NS`]，绝不与其他用户共享。
pub fn with_user_ns<T>(user_ns: &str, f: impl FnOnce() -> T) -> T {
    ACTIVE_USER_NS.with(|cell| {
        let previous = cell.replace(Some(user_ns.to_string()));
        let result = f();
        cell.replace(previous);
        result
    })
}

fn current_user_ns() -> String {
    ACTIVE_USER_NS
        .with(|cell| cell.borrow().clone())
        .unwrap_or_else(|| DEFAULT_USER_NS.to_string())
}

/// 给 KV 键加上命名空间前缀（用 Unit Separator 分隔，避免与业务键混淆）。
fn scoped_key(key: &str) -> String {
    format!("{}\u{1f}{}", current_user_ns(), key)
}

fn kv_get_scoped(key: &str) -> Option<String> {
    let map = JS_KV.lock().unwrap_or_else(|e| e.into_inner());
    map.get(&scoped_key(key)).cloned()
}

fn kv_put_scoped(key: &str, value: &str) {
    let mut map = JS_KV.lock().unwrap_or_else(|e| e.into_inner());
    map.insert(scoped_key(key), value.to_string());
}

/// 每个命名空间一个稳定的设备标识（模拟阅读 App 的 deviceID）。
fn device_id() -> String {
    const DEVICE_KEY: &str = "__device_id";
    if let Some(existing) = kv_get_scoped(DEVICE_KEY) {
        return existing;
    }
    let generated = Uuid::new_v4().to_string();
    kv_put_scoped(DEVICE_KEY, &generated);
    generated
}

/// 取当前命名空间的 JS HTTP 客户端（独立 Cookie jar）。
fn js_http_client() -> Client {
    let ns = current_user_ns();
    if let Ok(map) = JS_HTTP_CLIENTS.lock() {
        if let Some(client) = map.get(&ns) {
            return client.clone();
        }
    }
    let client = Client::builder()
        .cookie_store(true)
        .timeout(JS_HTTP_TIMEOUT)
        .gzip(true)
        .brotli(true)
        .deflate(true)
        // 与全局一致的重定向策略：限制跳数，开启防护时拒绝跳往内网
        .redirect(crate::crawler::url_guard::guarded_redirect_policy())
        .build()
        .expect("failed to build JS HTTP client");
    let mut map = JS_HTTP_CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(ns).or_insert(client).clone()
}

pub fn eval_js(script: &str, input: &str, base_url: &str) -> anyhow::Result<String> {
    eval_js_inner(script, Some(input), Some(base_url), None, None, None)
}

pub fn eval_js_with_bindings(
    script: &str,
    input: &str,
    base_url: &str,
    bindings: &HashMap<String, JsonValue>,
) -> anyhow::Result<String> {
    eval_js_inner(
        script,
        Some(input),
        Some(base_url),
        None,
        None,
        Some(bindings),
    )
}

pub fn eval_js_search_with_source(
    script: &str,
    key: &str,
    page: i32,
    source_key: &str,
) -> anyhow::Result<String> {
    eval_js_inner_with_source(
        script,
        None,
        None,
        Some(key),
        Some(page),
        Some(source_key),
        None,
    )
}

pub fn eval_js_url(
    script: &str,
    result: &str,
    key: &str,
    page: i32,
    source_key: &str,
    base_url: &str,
) -> anyhow::Result<String> {
    eval_js_inner_with_source(
        script,
        Some(result),
        Some(base_url),
        Some(key),
        Some(page),
        Some(source_key),
        None,
    )
}

fn eval_js_inner(
    script: &str,
    input: Option<&str>,
    base_url: Option<&str>,
    key: Option<&str>,
    page: Option<i32>,
    bindings: Option<&HashMap<String, JsonValue>>,
) -> anyhow::Result<String> {
    eval_js_inner_with_source(script, input, base_url, key, page, None, bindings)
}

fn eval_js_inner_with_source(
    script: &str,
    input: Option<&str>,
    base_url: Option<&str>,
    key: Option<&str>,
    page: Option<i32>,
    source_key: Option<&str>,
    bindings: Option<&HashMap<String, JsonValue>>,
) -> anyhow::Result<String> {
    let rt = Runtime::new()?;
    // 资源上限：内存 / 调用栈 / 执行时间（超时由中断处理器打断）
    rt.set_memory_limit(JS_MEMORY_LIMIT_BYTES);
    rt.set_max_stack_size(JS_MAX_STACK_BYTES);
    let deadline = Instant::now() + JS_EVAL_TIMEOUT;
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let ctx = Context::full(&rt)?;
    ctx.with(|ctx| {
        let globals = ctx.globals();
        let input_value = input.unwrap_or("");
        let base_url_value = base_url.unwrap_or("");
        let shared_js = active_js_lib_script()?;

        globals.set("input", input_value)?;
        globals.set("result", input_value)?;
        globals.set("src", input_value)?;
        globals.set("base_url", base_url_value)?;
        globals.set("baseUrl", base_url_value)?;
        if let Some(key) = key {
            globals.set("key", key)?;
        }
        if let Some(page) = page {
            globals.set("page", page)?;
        }

        // Default url variable for Legado compatibility
        globals.set("url", base_url_value)?;

        // Stubs for Legado compatibility
        let source_key_val = source_key.unwrap_or("").to_string();
        let source_obj = Object::new(ctx.clone())?;
        let sk_clone = source_key_val.clone();
        source_obj.set("key", source_key_val)?;
        source_obj.set("getKey", Func::new(move || sk_clone.clone()))?;
        globals.set("source", source_obj)?;

        let cookie_obj = Object::new(ctx.clone())?;
        cookie_obj.set(
            "removeCookie",
            Func::new(|_key: String| -> String { "".to_string() }),
        )?;
        globals.set("cookie", cookie_obj)?;

        let cache_obj = Object::new(ctx.clone())?;
        cache_obj.set(
            "get",
            Func::new(|key: String| -> Option<String> { kv_get_scoped(&key) }),
        )?;
        cache_obj.set(
            "put",
            Func::new(|key: String, val: String| -> bool {
                kv_put_scoped(&key, &val);
                true
            }),
        )?;
        globals.set("cache", cache_obj)?;

        let java_obj = Object::new(ctx.clone())?;
        java_obj.set(
            "ajax",
            Func::new(|spec: String| -> String { java_ajax(&spec).unwrap_or_default() }),
        )?;
        java_obj.set(
            "md5Encode",
            Func::new(|input: String| -> String { md5_hex(&input) }),
        )?;
        java_obj.set(
            "timeFormat",
            Func::new(|timestamp: i64| -> String { java_time_format(timestamp) }),
        )?;
        java_obj.set("androidId", Func::new(|| -> String { device_id() }))?;
        java_obj.set("deviceID", Func::new(|| -> String { device_id() }))?;
        java_obj.set(
            "get",
            Func::new(|url: String| -> String {
                java_request_simple("GET", &url, None).unwrap_or_default()
            }),
        )?;
        java_obj.set(
            "post",
            Func::new(|url: String, body: String| -> String {
                java_request_simple("POST", &url, Some(body)).unwrap_or_default()
            }),
        )?;
        java_obj.set(
            "put",
            Func::new(|url: String, body: String| -> String {
                java_request_simple("PUT", &url, Some(body)).unwrap_or_default()
            }),
        )?;
        java_obj.set(
            "base64Encode",
            Func::new(|input: String| -> String {
                base64::engine::general_purpose::STANDARD.encode(input)
            }),
        )?;
        java_obj.set(
            "base64Decode",
            Func::new(|input: String| -> String {
                base64::engine::general_purpose::STANDARD
                    .decode(input)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or_default()
            }),
        )?;
        java_obj.set(
            "aesBase64DecodeToString",
            Func::new(
                |input: String, key: String, algorithm: String, iv: String| -> String {
                    java_aes_base64_decode_to_string(&input, &key, &algorithm, &iv)
                },
            ),
        )?;
        java_obj.set(
            "encodeURIComponent",
            Func::new(|input: String| -> String { urlencoding::encode(&input).into_owned() }),
        )?;
        java_obj.set(
            "decodeURIComponent",
            Func::new(|input: String| -> String {
                urlencoding::decode(&input)
                    .map(|s| s.into_owned())
                    .unwrap_or_default()
            }),
        )?;
        java_obj.set(
            "encodeURI",
            Func::new(|input: String| -> String { urlencoding::encode(&input).into_owned() }),
        )?;
        java_obj.set(
            "decodeURI",
            Func::new(|input: String| -> String {
                urlencoding::decode(&input)
                    .map(|s| s.into_owned())
                    .unwrap_or_default()
            }),
        )?;
        java_obj.set(
            "now",
            Func::new(|| -> i64 { chrono::Utc::now().timestamp_millis() }),
        )?;
        java_obj.set(
            "uuid",
            Func::new(|| -> String { Uuid::new_v4().to_string() }),
        )?;
        globals.set("java", java_obj)?;

        globals.set(
            "kv_get",
            Func::new(|key: String| -> Option<String> { kv_get_scoped(&key) }),
        )?;
        globals.set(
            "kv_put",
            Func::new(|key: String, val: String| -> bool {
                kv_put_scoped(&key, &val);
                true
            }),
        )?;
        globals.set(
            "regex_replace",
            Func::new(
                |input: String, pattern: String, replace: String| -> String {
                    apply_regex_replace(&input, &pattern, &replace)
                },
            ),
        )?;
        globals.set(
            "strip_ws",
            Func::new(|input: String| -> String { strip_whitespace(&input) }),
        )?;

        globals.set("book", Object::new(ctx.clone())?)?;
        globals.set("chapter", Object::new(ctx.clone())?)?;
        globals.set("title", "")?;
        globals.set("nextChapterUrl", "")?;
        globals.set("rssArticle", Object::new(ctx.clone())?)?;

        if let Some(bindings) = bindings {
            for (key, value) in bindings {
                let js_value = ctx.json_parse(value.to_string())?;
                globals.set(key.as_str(), js_value)?;
            }
        }

        if !shared_js.trim().is_empty() {
            eval_script(ctx.clone(), &shared_js)?;
        }

        let v = eval_script(ctx.clone(), script)?;

        let result = if v.is_null() || v.is_undefined() {
            String::new()
        } else if let Some(s) = v.clone().into_string() {
            let s: rquickjs::String<'_> = s;
            s.to_string()
                .map(|value| value.to_string())
                .unwrap_or_default()
        } else {
            match ctx.json_stringify(v) {
                Ok(Some(json)) => json.to_string().unwrap_or_default(),
                _ => String::new(),
            }
        };
        if result.len() > JS_MAX_RESULT_BYTES {
            anyhow::bail!("JS 返回结果过大: {} 字节", result.len());
        }
        Ok(result)
    })
}

fn java_aes_base64_decode_to_string(input: &str, key: &str, algorithm: &str, iv: &str) -> String {
    let algorithm = algorithm.to_ascii_uppercase();
    if algorithm != "AES/CBC/PKCS5PADDING" && algorithm != "AES/CBC/PKCS7PADDING" {
        return String::new();
    }

    let Ok(mut encrypted) = base64::engine::general_purpose::STANDARD.decode(input.trim()) else {
        return String::new();
    };

    let Ok(cipher) = Aes128CbcDecryptor::new_from_slices(key.as_bytes(), iv.as_bytes()) else {
        return String::new();
    };

    cipher
        .decrypt_padded_mut::<Pkcs7>(&mut encrypted)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
        .unwrap_or_default()
}

fn eval_script<'js>(ctx: rquickjs::Ctx<'js>, script: &str) -> anyhow::Result<Value<'js>> {
    match ctx.eval(script) {
        Ok(v) => Ok(v),
        Err(e) => {
            if let Some(exception) = ctx.catch().into_exception() {
                return Err(anyhow::anyhow!("JS Exception: {:?}", exception));
            }
            Err(e.into())
        }
    }
}

fn active_js_lib_script() -> anyhow::Result<String> {
    let js_lib = ACTIVE_JS_LIB.with(|cell| cell.borrow().clone());
    let Some(js_lib) = js_lib.filter(|value| !value.trim().is_empty()) else {
        return Ok(String::new());
    };
    let cache_key = md5_hex(&js_lib);
    if let Some(cached) = JS_LIB_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
        .cloned()
    {
        return Ok(cached);
    }

    let compiled = compile_js_lib(&js_lib)?;
    JS_LIB_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(cache_key, compiled.clone());
    Ok(compiled)
}

fn compile_js_lib(js_lib: &str) -> anyhow::Result<String> {
    let trimmed = js_lib.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<JsonValue>(trimmed) {
            if let Some(map) = value.as_object() {
                let mut scripts = Vec::new();
                for entry in map.values() {
                    if let Some(raw) = entry.as_str() {
                        scripts.push(resolve_js_lib_entry(raw)?);
                    }
                }
                return Ok(scripts.join("\n"));
            }
        }
    }
    Ok(trimmed.to_string())
}

fn resolve_js_lib_entry(entry: &str) -> anyhow::Result<String> {
    let value = entry.trim();
    if value.starts_with("http://") || value.starts_with("https://") {
        // 出站守卫：书源 jsLib 可以任意指定 URL，不校验即是盲打内网的 SSRF 通道
        crate::crawler::url_guard::ensure_outbound_url_str_allowed_blocking(value)
            .map_err(|reason| anyhow::anyhow!("jsLib 远程拉取被出站策略拒绝: {reason}"))?;
        let response = js_http_client().get(value).send()?;
        return read_blocking_body_limited(response);
    }
    Ok(value.to_string())
}

/// JS 侧 HTTP 响应体的限量读取：JS 堆 128MB 上限管不到 Rust 侧的响应缓冲，
/// 无界 text() 会让恶意书源用超大响应直接耗尽进程内存。
fn read_blocking_body_limited(response: reqwest::blocking::Response) -> anyhow::Result<String> {
    use std::io::Read;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let limit = crate::crawler::fetcher::MAX_RESPONSE_BYTES;
    let mut buf = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > limit {
        anyhow::bail!("JS HTTP 响应体超过 {limit} 字节上限");
    }
    Ok(crate::crawler::fetcher::decode_body(
        &buf,
        None,
        content_type.as_deref(),
    ))
}

fn java_time_format(timestamp: i64) -> String {
    let secs = if timestamp > 1_000_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    };
    match Local.timestamp_opt(secs, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

fn java_ajax(spec: &str) -> anyhow::Result<String> {
    let (url, options) = split_ajax_spec(spec);
    if url.trim().is_empty() {
        return Ok(String::new());
    }
    // 出站守卫：书源脚本可以任意指定 URL
    if crate::crawler::url_guard::ensure_outbound_url_str_allowed_blocking(url).is_err() {
        return Ok(String::new());
    }

    let options_json = options
        .and_then(|raw| serde_json::from_str::<JsonValue>(raw).ok())
        .unwrap_or(JsonValue::Null);

    let method = options_json
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_uppercase();
    let method = Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET);

    let mut req = js_http_client().request(method, url.trim());

    if let Some(headers) = options_json.get("headers").and_then(|v| v.as_object()) {
        for (key, value) in headers {
            if let Some(value) = value.as_str() {
                req = req.header(key, value);
            } else if !value.is_null() {
                req = req.header(key, value.to_string());
            }
        }
    }

    if let Some(body) = options_json.get("body") {
        if let Some(body) = body.as_str() {
            req = req.body(body.to_string());
        } else if !body.is_null() {
            req = req.body(body.to_string());
        }
    }

    let response = req.send()?;
    read_blocking_body_limited(response)
}

fn java_request_simple(method: &str, url: &str, body: Option<String>) -> anyhow::Result<String> {
    // 出站守卫：书源脚本可以任意指定 URL
    if crate::crawler::url_guard::ensure_outbound_url_str_allowed_blocking(url).is_err() {
        return Ok(String::new());
    }
    let method = Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET);
    let mut req = js_http_client().request(method, url.trim());
    if let Some(body) = body {
        req = req.body(body);
    }
    let response = req.send()?;
    read_blocking_body_limited(response)
}

fn split_ajax_spec(spec: &str) -> (&str, Option<&str>) {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut quote = '\0';
    let mut escaped = false;

    for (idx, ch) in spec.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_string => {
                escaped = true;
            }
            '"' | '\'' if in_string && ch == quote => {
                in_string = false;
                quote = '\0';
            }
            '"' | '\'' if !in_string => {
                in_string = true;
                quote = ch;
            }
            '{' | '[' if !in_string => depth += 1,
            '}' | ']' if !in_string => depth -= 1,
            ',' if !in_string && depth == 0 => {
                let left = &spec[..idx];
                let right = &spec[idx + ch.len_utf8()..];
                return (left, Some(right.trim()));
            }
            _ => {}
        }
    }

    (spec, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// 带看门狗的求值。JS 沙箱若失去超时能力，测试会失败而不是永久挂死整个
    /// `cargo test`（泄漏一个线程，但能拿到明确结论）。
    fn eval_watched(script: &str, wait: Duration) -> Option<anyhow::Result<String>> {
        let (tx, rx) = mpsc::channel();
        let script = script.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(eval_js(&script, "", ""));
        });
        rx.recv_timeout(wait).ok()
    }

    #[test]
    fn normal_js_still_works() {
        assert_eq!(eval_js("'hello' + ' ' + 'world'", "", "").unwrap(), "hello world");
        // 字符串方法 / 正则等常用能力不受资源上限影响
        assert_eq!(eval_js("'a,b,c'.split(',').length", "", "").unwrap(), "3");
    }

    #[test]
    fn infinite_loop_is_interrupted_not_hung() {
        // 书源里的 while(true){} 必须被中断处理器在超时后打断
        match eval_watched("while(true){}", Duration::from_secs(20)) {
            None => panic!("JS 死循环未被中断处理器打断：20s 内未返回"),
            Some(Ok(v)) => panic!("JS 死循环竟然返回成功: {v:?}"),
            Some(Err(_)) => {}
        }
    }

    #[test]
    fn unbounded_allocation_is_rejected() {
        // 持续分配必须撞上 128MB 内存上限（或先撞 5s 超时），而不是打爆进程
        let script = "var a=[]; while(true){ a.push(new Array(65536).fill(1)); }";
        match eval_watched(script, Duration::from_secs(30)) {
            None => panic!("JS 无限分配未被内存上限或超时拦住：30s 内未返回"),
            Some(Ok(v)) => panic!("JS 无限分配竟然返回成功: {v:?}"),
            Some(Err(_)) => {}
        }
    }

    #[test]
    fn kv_is_isolated_between_users() {
        with_user_ns("alice", || kv_put_scoped("shared", "alice-value"));
        // bob 既读不到 alice 的值，也覆盖不了它
        with_user_ns("bob", || {
            assert_eq!(kv_get_scoped("shared"), None, "bob 不应读到 alice 的键");
            kv_put_scoped("shared", "bob-value");
        });
        with_user_ns("alice", || {
            assert_eq!(
                kv_get_scoped("shared").as_deref(),
                Some("alice-value"),
                "alice 的值不应被 bob 覆盖"
            );
        });
        // 未设置命名空间时归入 public，同样与具名用户隔离
        assert_eq!(kv_get_scoped("shared"), None);
    }

    #[test]
    fn device_id_is_stable_per_user_and_distinct_across_users() {
        let alice_first = with_user_ns("alice", device_id);
        let alice_second = with_user_ns("alice", device_id);
        let bob = with_user_ns("bob", device_id);
        assert_eq!(alice_first, alice_second, "同一用户的 deviceID 应保持稳定");
        assert_ne!(alice_first, bob, "不同用户不应共享 deviceID");
    }

    #[test]
    fn js_cache_binding_is_isolated_between_users() {
        with_user_ns("alice", || {
            eval_js("cache.put('token', 'from-alice')", "", "").unwrap();
        });
        let miss_for_bob = with_user_ns("bob", || {
            eval_js(
                "(cache.get('token') == null) ? 'MISS' : 'HIT'",
                "",
                "",
            )
            .unwrap()
        });
        assert_eq!(miss_for_bob, "MISS", "bob 不应读到 alice 写入的 cache");
        let hit_for_alice = with_user_ns("alice", || {
            eval_js("String(cache.get('token'))", "", "").unwrap()
        });
        assert_eq!(hit_for_alice, "from-alice");
    }
}
