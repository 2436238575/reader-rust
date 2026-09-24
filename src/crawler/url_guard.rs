//! 出站请求守卫（SSRF 防护）。
//!
//! 本项目里「由用户提供的 URL → 服务端发起请求」的入口很多：书源抓取、封面抓取、
//! 远程书源导入、`bookSourceProxy`、`aiProxy` 等。如果没有约束，它们就是一个
//! 可以被用来探测/读取内网（云元数据 `169.254.169.254`、本地管理端口等）的代理。
//!
//! 默认**放行**私网地址（`ALLOW_PRIVATE_NETWORK=true`）：本项目默认按自托管单用户
//! 场景使用，局域网书源、本地书源服务（如 `http://192.168.x.x:9999`）、本地模型服务
//! 都属正常用法，参考实现（阅读/Legado）同样不做限制。
//!
//! 多用户或公网暴露的部署应显式设为 `ALLOW_PRIVATE_NETWORK=false`，此时会拦截私网、
//! 环回、链路本地、ULA、CGNAT 等地址，并禁止 302 跳转到这些地址——否则任意用户都能
//! 把服务端当成内网探测代理。
//!
//! 命中策略时抛出 [`OutboundBlocked`]，上层据此回一个带原因的 4xx，而不是
//! 被兜底成看不出所以然的 "internal error"。

use reqwest::redirect::Policy;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use url::{Host, Url};

/// 一次请求最多跟随的重定向跳数。
const MAX_REDIRECTS: usize = 5;

/// 与 `AppConfig::allow_private_network` 的默认值保持一致（自托管单用户放行私网）。
static ALLOW_PRIVATE_NETWORK: AtomicBool = AtomicBool::new(true);

/// 由 bootstrap 按配置初始化。
pub fn set_allow_private_network(allow: bool) {
    ALLOW_PRIVATE_NETWORK.store(allow, Ordering::Relaxed);
}

pub fn private_network_allowed() -> bool {
    ALLOW_PRIVATE_NETWORK.load(Ordering::Relaxed)
}

/// 出站策略拒绝。
///
/// 单独一个类型而不是裸字符串：上层必须把「策略拒绝」与真正的内部故障区分开——
/// 前者是配置或输入问题，原因要原样回给调用方；被兜底成 "internal error" 的话
/// 用户完全无从判断（书源指向内网时尤其容易踩到）。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct OutboundBlocked(pub String);

/// 该 IP 是否属于禁止出站访问的范围。
pub fn is_forbidden_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => {
            // IPv4-mapped（::ffff:169.254.169.254 这类）按 IPv4 规则判定
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden_v4(&v4);
            }
            is_forbidden_v6(v6)
        }
    }
}

fn is_forbidden_v4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()          // 10/8, 172.16/12, 192.168/16
        || ip.is_loopback()  // 127/8
        || ip.is_link_local()// 169.254/16（含云元数据 169.254.169.254）
        || ip.is_unspecified()
        || ip.is_multicast()
        || o[0] == 0                                   // 0.0.0.0/8 "this network"
        || o == [255, 255, 255, 255]                   // 受限广播
        || (o[0] == 100 && (o[1] & 0xC0) == 64)        // 100.64/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)     // 192.0.0.0/24
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)     // 192.0.2.0/24 TEST-NET-1
        || (o[0] == 198 && (o[1] & 0xFE) == 18)        // 198.18/15 基准测试
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)  // 198.51.100.0/24 TEST-NET-2
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)   // 203.0.113.0/24 TEST-NET-3
        || o[0] >= 240 // 240/4 保留
}

fn is_forbidden_v6(ip: &Ipv6Addr) -> bool {
    let s = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (s[0] & 0xFFC0) == 0xFE80  // fe80::/10 链路本地
        || (s[0] & 0xFE00) == 0xFC00  // fc00::/7 唯一本地地址
        || (s[0] & 0xFFC0) == 0xFEC0 // fec0::/10 站点本地（已废弃但仍是内网语义）
}

/// 明显指向内网的主机名（含常见的云元数据别名）。
pub fn is_forbidden_hostname(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h.ends_with(".local")
        || h.ends_with(".internal")
        || h.ends_with(".intranet")
        || h.ends_with(".lan")
        || h.ends_with(".home.arpa")
        || h == "metadata"
        || h == "instance-data"
        || h.ends_with(".metadata.google.internal")
}

/// 同步（不做 DNS）判断 URL 是否明显指向内网。用于重定向逐跳校验。
pub fn is_obviously_internal_url(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => is_forbidden_ip(&IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => is_forbidden_ip(&IpAddr::V6(ip)),
        Some(Host::Domain(domain)) => is_forbidden_hostname(domain),
        None => true,
    }
}

/// 协议与内网主机名检查，并返回用于 DNS 解析的主机字符串。
///
/// 注意：`Url::host_str()` 对 IPv6 返回带方括号的形式（`[::1]`），
/// 所以这里统一走 `Url::host()` 枚举，避免把 IPv6 字面量误当成域名漏检。
fn outbound_lookup_host(url: &Url) -> Result<String, String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("不支持的协议: {}", url.scheme()));
    }
    match url.host() {
        Some(Host::Ipv4(ip)) => Ok(ip.to_string()),
        Some(Host::Ipv6(ip)) => Ok(ip.to_string()),
        Some(Host::Domain(domain)) => {
            if is_forbidden_hostname(domain) {
                return Err(format!("禁止访问内网主机: {domain}"));
            }
            Ok(domain.to_string())
        }
        None => Err("URL 缺少主机名".to_string()),
    }
}

/// 逐个检查解析出的 IP，任一命中禁用网段即拒绝。
fn check_resolved_ips(ips: impl Iterator<Item = IpAddr>) -> Result<(), String> {
    let mut resolved = false;
    for ip in ips {
        resolved = true;
        if is_forbidden_ip(&ip) {
            return Err(format!("禁止访问内网地址: {ip}"));
        }
    }
    if !resolved {
        return Err("无法解析主机".to_string());
    }
    Ok(())
}

/// 校验一个用户可控的出站 URL：协议 + 主机名 + 解析出的全部 IP。
///
/// `ALLOW_PRIVATE_NETWORK` 打开时跳过 DNS/内网判定直接放行（本地单用户模式，
/// 零额外开销），仅协议白名单仍然生效。
pub async fn ensure_outbound_url_allowed(url: &Url) -> Result<(), String> {
    // 协议检查放在放行早退之前：即使放行私网，file:/ftp: 之类也不该交给
    // 下游（reqwest 会兜底拒绝，但守卫自身保持同一语义，且报错更可读）
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("不支持的协议: {}", url.scheme()));
    }
    if private_network_allowed() {
        return Ok(());
    }
    let host = outbound_lookup_host(url)?;
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| format!("无法解析主机 {host}: {e}"))?;
    check_resolved_ips(addrs.map(|addr| addr.ip()))
}

/// 便捷入口：解析字符串后校验。
pub async fn ensure_outbound_url_str_allowed(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|e| format!("URL 解析失败: {e}"))?;
    ensure_outbound_url_allowed(&url).await?;
    Ok(url)
}

/// 同步版本：供运行在阻塞上下文中的调用方使用（例如 JS 运行时的 `java.ajax`）。
///
/// 因为 JS 求值本身是同步阻塞的，这里直接用阻塞式 DNS 解析（`to_socket_addrs`），
/// 语义与异步版本一致。
pub fn ensure_outbound_url_str_allowed_blocking(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|e| format!("URL 解析失败: {e}"))?;
    if private_network_allowed() {
        return Ok(url);
    }
    let host = outbound_lookup_host(&url)?;
    let port = url.port_or_known_default().unwrap_or(80);
    use std::net::ToSocketAddrs;
    let addrs = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("无法解析主机 {host}: {e}"))?;
    check_resolved_ips(addrs.map(|addr| addr.ip()))?;
    Ok(url)
}

/// 校验重定向目标。不能只看字面 IP/主机名：302 目标的域名同样可能解析到内网
/// （攻击者把自家域名 A 记录指向 127.0.0.1），域名主机必须做真实 DNS 解析。
fn check_redirect_target(url: &Url) -> Result<(), String> {
    if private_network_allowed() {
        return Ok(());
    }
    if is_obviously_internal_url(url) {
        return Err("重定向目标指向内网地址".to_string());
    }
    if let Some(Host::Domain(domain)) = url.host() {
        use std::net::ToSocketAddrs;
        let port = url.port_or_known_default().unwrap_or(80);
        let addrs = (domain, port)
            .to_socket_addrs()
            .map_err(|e| format!("重定向目标 {domain} 解析失败: {e}"))?;
        check_resolved_ips(addrs.map(|addr| addr.ip()))?;
    }
    Ok(())
}

/// 重定向策略：限制跳数；开启防护时拒绝跳往内网地址。
pub fn guarded_redirect_policy() -> Policy {
    Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        if let Err(reason) = check_redirect_target(attempt.url()) {
            return attempt.error(format!("redirect blocked: {reason}"));
        }
        attempt.follow()
    })
}

/// 带守卫的基础 client builder：统一超时、UA 与重定向策略。
pub fn guarded_client_builder(timeout: Duration, user_agent: &str) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(user_agent.to_string())
        .redirect(guarded_redirect_policy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_and_metadata_ips() {
        for ip in [
            "127.0.0.1",
            "0.0.0.0",
            "10.1.2.3",
            "172.16.5.4",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "255.255.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:169.254.169.254",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(is_forbidden_ip(&ip), "{ip} 应被拦截");
        }
    }

    #[test]
    fn allows_public_ips() {
        for ip in ["1.1.1.1", "8.8.8.8", "93.184.216.34", "2606:4700::1111"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(!is_forbidden_ip(&ip), "{ip} 不应被拦截");
        }
    }

    #[test]
    fn blocks_internal_hostnames() {
        for host in [
            "localhost",
            "foo.localhost",
            "router.local",
            "svc.internal",
            "nas.lan",
            "metadata",
            "metadata.google.internal",
        ] {
            assert!(is_forbidden_hostname(host), "{host} 应被拦截");
        }
        for host in ["example.com", "api.openai.com", "localhost.example.com"] {
            assert!(!is_forbidden_hostname(host), "{host} 不应被拦截");
        }
    }

    #[test]
    fn obviously_internal_url_detection() {
        assert!(is_obviously_internal_url(
            &Url::parse("http://169.254.169.254/latest/meta-data/").unwrap()
        ));
        assert!(is_obviously_internal_url(
            &Url::parse("http://[::1]:8080/").unwrap()
        ));
        assert!(is_obviously_internal_url(
            &Url::parse("http://localhost:6379/").unwrap()
        ));
        assert!(!is_obviously_internal_url(
            &Url::parse("https://example.com/x").unwrap()
        ));
    }

    #[test]
    fn redirect_target_check_blocks_literal_internal() {
        // 默认 SECURE=false → 放行私网，先强制切到防护态再测
        set_allow_private_network(false);
        assert!(check_redirect_target(&Url::parse("http://127.0.0.1/").unwrap()).is_err());
        assert!(check_redirect_target(&Url::parse("http://[::1]/").unwrap()).is_err());
        assert!(check_redirect_target(&Url::parse("http://localhost/").unwrap()).is_err());
        // 守护状态下 localhost 既命中主机名黑名单，也过不了 DNS 解析检查
        set_allow_private_network(true);
        assert!(check_redirect_target(&Url::parse("http://127.0.0.1/").unwrap()).is_ok());
    }
}
