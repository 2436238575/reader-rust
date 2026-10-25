//! HttpOnly Cookie 会话工具。
//!
//! Access / Refresh 令牌从 JSON 响应迁移到 HttpOnly Cookie：JS 读不到，
//! XSS 也偷不走。两枚 Cookie 都是 host-only（不设 `Domain`）、
//! `Path=/reader3`、`SameSite=Strict`；`Secure` 由 `COOKIE_SECURE`
//! 三态控制，默认 `auto` 按请求 Host 判定（回环主机不加，其余一律加）。

use axum::http::{header, HeaderMap, HeaderValue};

/// Access Token 的 Cookie 名。
pub const ACCESS_COOKIE: &str = "reader_access";
/// Refresh Token 的 Cookie 名。
pub const REFRESH_COOKIE: &str = "reader_refresh";

/// Cookie 只随 `/reader3` 前缀的 API 请求发送，静态资源不带。
const COOKIE_PATH: &str = "/reader3";

/// 从请求头解析指定名称的 Cookie 值。
///
/// 手写分号分隔解析（不引 cookie crate）：本服务的 Cookie 值只有
/// JWT（base64url + `.`）与 `rt_` 前缀随机串，不含分号/引号等需要
/// RFC 6265 完整实现的字符。多条 `Cookie` 头都参与匹配，返回首个
/// 非空值。
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|raw| raw.split(';'))
        .find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.trim() == name).then(|| value.trim().to_string())
        })
        .filter(|value| !value.is_empty())
}

/// 生成下发会话 Cookie 的 `Set-Cookie` 值。
///
/// `max_age` 单位秒：Access / Refresh 分别取各自 TTL，刷新时重新计时。
pub fn build_cookie(name: &str, value: &str, max_age: u64, secure: bool) -> HeaderValue {
    let mut cookie =
        format!("{name}={value}; Max-Age={max_age}; Path={COOKIE_PATH}; HttpOnly; SameSite=Strict");
    if secure {
        cookie.push_str("; Secure");
    }
    // 名称固定、值是 base64url/JWT 字符集，均在 HeaderValue 合法范围内
    HeaderValue::from_str(&cookie).expect("会话 Cookie 值应在 HeaderValue 合法范围内")
}

/// 生成清除会话 Cookie 的 `Set-Cookie` 值。
///
/// `Max-Age=0` 且其余属性与下发时一致（Path/HttpOnly/SameSite/Secure），
/// 浏览器按同名同路径覆盖删除；属性不一致会留下孤儿 Cookie。
pub fn clear_cookie(name: &str, secure: bool) -> HeaderValue {
    build_cookie(name, "", 0, secure)
}

/// `COOKIE_SECURE` 三态开关：
///
/// - `auto`（默认）：请求 Host 是回环地址（`localhost` / `127.0.0.1` /
///   `[::1]`）时不加 `Secure`——本地开发走 HTTP，加了浏览器会拒收；
///   其余一律加。
/// - `true` / `false`：强制（配置值已在启动阶段归一为小写）。
pub fn cookie_secure(mode: &str, host_header: Option<&str>) -> bool {
    match mode {
        "true" => true,
        "false" => false,
        _ => !is_loopback_host(host_header.map(host_without_port).unwrap_or_default()),
    }
}

/// 判断 Host（不带端口）是否属于本地回环。
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // IPv6 字面量带方括号（`[::1]`），剥掉后统一比较
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare == "127.0.0.1" || bare == "::1"
}

/// 从 `Host` 头剥掉端口：`localhost:8080` → `localhost`，`[::1]:8080` → `[::1]`。
///
/// IPv6 字面量自带冒号，必须按 `]` 右边界切；未加方括号的裸 IPv6
///（非规范形态，契约里仍要求识别）按「多个冒号」识别并原样保留。
fn host_without_port(host: &str) -> &str {
    if let Some(end) = host.find(']') {
        return &host[..=end];
    }
    if host.matches(':').count() > 1 {
        return host;
    }
    match host.rfind(':') {
        Some(index) => &host[..index],
        None => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with_cookie(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::COOKIE, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn parses_single_and_multiple_cookies() {
        let headers = headers_with_cookie(&["a=1; reader_access=tok.a.b; b=2"]);
        assert_eq!(
            cookie_value(&headers, ACCESS_COOKIE).as_deref(),
            Some("tok.a.b")
        );
        assert_eq!(cookie_value(&headers, "a").as_deref(), Some("1"));
        assert_eq!(cookie_value(&headers, "b").as_deref(), Some("2"));
        assert_eq!(cookie_value(&headers, "missing"), None);
    }

    #[test]
    fn parses_across_multiple_cookie_headers() {
        // RFC 6265 允许多条 Cookie 头，取第一个命中
        let headers = headers_with_cookie(&["a=1", "reader_access=from-second"]);
        assert_eq!(
            cookie_value(&headers, ACCESS_COOKIE).as_deref(),
            Some("from-second")
        );
    }

    #[test]
    fn name_must_match_exactly() {
        // 前缀相同的名字不能误命中
        let headers = headers_with_cookie(&["reader_accessX=other; reader_refresh=rt_ok"]);
        assert_eq!(cookie_value(&headers, ACCESS_COOKIE), None);
        assert_eq!(
            cookie_value(&headers, REFRESH_COOKIE).as_deref(),
            Some("rt_ok")
        );
    }

    #[test]
    fn empty_value_is_treated_as_missing() {
        let headers = headers_with_cookie(&["reader_access=; other=1"]);
        assert_eq!(cookie_value(&headers, ACCESS_COOKIE), None);
    }

    #[test]
    fn build_cookie_carries_session_attributes() {
        let header = build_cookie(ACCESS_COOKIE, "tok", 21600, false);
        let cookie = header.to_str().unwrap();
        assert!(cookie.starts_with("reader_access=tok;"));
        assert!(cookie.contains("Max-Age=21600"));
        assert!(cookie.contains("Path=/reader3"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(!cookie.contains("Secure"), "非安全模式不应带 Secure");
        assert!(
            !cookie.contains("Domain="),
            "host-only Cookie 不能设 Domain"
        );
    }

    #[test]
    fn build_cookie_appends_secure_when_requested() {
        let header = build_cookie(REFRESH_COOKIE, "rt_x", 604800, true);
        let cookie = header.to_str().unwrap();
        assert!(cookie.ends_with("; Secure"));
    }

    #[test]
    fn clear_cookie_expires_with_same_attributes() {
        let header = clear_cookie(ACCESS_COOKIE, true);
        let cookie = header.to_str().unwrap();
        assert!(cookie.starts_with("reader_access=;"));
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("Path=/reader3"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Secure"));
    }

    #[test]
    fn cookie_secure_mode_overrides_host_detection() {
        assert!(cookie_secure("true", Some("localhost")));
        assert!(!cookie_secure("false", Some("example.org")));
    }

    #[test]
    fn cookie_secure_auto_follows_loopback_host() {
        for host in [
            "localhost",
            "LOCALHOST:8080",
            "127.0.0.1",
            "[::1]:8080",
            "::1",
        ] {
            assert!(
                !cookie_secure("auto", Some(host)),
                "回环 Host {host} 不应加 Secure"
            );
        }
        for host in [
            "example.org",
            "192.168.1.5:8080",
            "nas.lan",
            "[fe80::1]:8080",
        ] {
            assert!(
                cookie_secure("auto", Some(host)),
                "非回环 Host {host} 应加 Secure"
            );
        }
        // 没有 Host 头（HTTP/1.0 或异常请求）按非回环处理，安全优先
        assert!(cookie_secure("auto", None));
    }

    #[test]
    fn host_without_port_handles_ipv4_ipv6_and_bare_hosts() {
        assert_eq!(host_without_port("localhost:8080"), "localhost");
        assert_eq!(host_without_port("localhost"), "localhost");
        assert_eq!(host_without_port("[::1]:8080"), "[::1]");
        assert_eq!(host_without_port("[::1]"), "[::1]");
        assert_eq!(host_without_port("127.0.0.1:9000"), "127.0.0.1");
    }

    #[test]
    fn loopback_host_matches_the_contracted_set() {
        for host in ["localhost", "LocalHost", "127.0.0.1", "::1", "[::1]"] {
            assert!(is_loopback_host(host), "{host} 应视为回环");
        }
        for host in ["example.org", "192.168.1.1", "127.0.0.2", "fe80::1", ""] {
            assert!(!is_loopback_host(host), "{host} 不应视为回环");
        }
    }
}
