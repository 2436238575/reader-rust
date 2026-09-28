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
//! 多用户或公网暴露的部署应显式设为 `ALLOW_PRIVATE_NETWORK=false` 并配置
//! `PRIVATE_NETWORK_WHITELIST` 白名单，此时会拦截私网、环回、链路本地、ULA、
//! CGNAT 等地址，并禁止 302 跳转到这些地址——否则任意用户都能把服务端当成
//! 内网探测代理。
//!
//! 白名单语义：`ALLOW_PRIVATE_NETWORK=false` 启用白名单机制，**名单为空时全部
//! 放行**（等同 `true`，方便先设 false 再逐步收紧），名单非空时只有命中的目标
//! 可以出站。条目支持 IP、CIDR 网段与域名，均可带端口。
//!
//! 命中策略时抛出 [`OutboundBlocked`]，上层据此回一个带原因的 4xx，而不是
//! 被兜底成看不出所以然的 "internal error"。

use reqwest::redirect::Policy;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;
use std::time::Duration;
use url::{Host, Url};

/// 一次请求最多跟随的重定向跳数。
const MAX_REDIRECTS: usize = 5;

/// 与 `AppConfig::allow_private_network` 的默认值保持一致（自托管单用户放行私网）。
static ALLOW_PRIVATE_NETWORK: AtomicBool = AtomicBool::new(true);

/// 私网白名单（`PRIVATE_NETWORK_WHITELIST`），由 bootstrap 按配置初始化。
/// 读多写少（只在启动时写一次、测试里偶尔重置），用 RwLock 而不是 OnceLock。
static PRIVATE_WHITELIST: RwLock<Vec<PrivateTarget>> = RwLock::new(Vec::new());

/// 由 bootstrap 按配置初始化。
pub fn set_allow_private_network(allow: bool) {
    ALLOW_PRIVATE_NETWORK.store(allow, Ordering::Relaxed);
}

pub fn private_network_allowed() -> bool {
    ALLOW_PRIVATE_NETWORK.load(Ordering::Relaxed)
}

/// 白名单条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivateTarget {
    /// 单个 IP，可带端口约束。
    Ip { ip: IpAddr, port: Option<u16> },
    /// CIDR 网段（只对 IP 目标有意义，域名条目不走网段）。
    Cidr { net: IpAddr, prefix: u8 },
    /// 域名（小写、不带尾点），可带端口约束。
    Domain { name: String, port: Option<u16> },
}

/// 解析白名单配置串（逗号分隔）。
///
/// 条目形态：`192.168.100.99`、`192.168.100.0/24`、`nas.lan`，均可带端口
/// （`192.168.100.99:9999`；IPv6 带端口写作 `[::1]:8080`）。解析失败的条目
/// 直接报错——静默忽略会让「收紧」悄悄变成「放行」。
pub fn parse_private_whitelist(raw: &str) -> Result<Vec<PrivateTarget>, String> {
    let mut targets = Vec::new();
    for part in raw.split(',') {
        let entry = part.trim();
        if entry.is_empty() {
            continue;
        }
        targets.push(parse_private_target(entry)?);
    }
    Ok(targets)
}

fn parse_private_target(entry: &str) -> Result<PrivateTarget, String> {
    // CIDR：先于 host:port 判断（`10.0.0.0/8` 不含端口语义）
    if let Some((addr, prefix)) = entry.rsplit_once('/') {
        let ip: IpAddr = addr
            .parse()
            .map_err(|_| format!("白名单网段无效: {entry}"))?;
        let max = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let prefix: u8 = prefix
            .parse()
            .map_err(|_| format!("白名单网段前缀无效: {entry}"))?;
        if prefix > max {
            return Err(format!("白名单网段前缀超出范围: {entry}"));
        }
        return Ok(PrivateTarget::Cidr { net: ip, prefix });
    }
    // host[:port]；IPv6 字面量带端口必须是 `[::1]:8080` 形态
    let (host, port) = if let Some(rest) = entry.strip_prefix('[') {
        let (v6, tail) = rest
            .split_once(']')
            .ok_or_else(|| format!("白名单条目无效: {entry}"))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => Some(
                p.parse::<u16>()
                    .map_err(|_| format!("白名单端口无效: {entry}"))?,
            ),
            None if tail.is_empty() => None,
            None => return Err(format!("白名单条目无效: {entry}")),
        };
        (v6, port)
    } else if entry.matches(':').count() > 1 {
        // 裸 IPv6（`::1`、`fd00::1`）不含端口语义
        (entry, None)
    } else if let Some((h, p)) = entry.rsplit_once(':') {
        let port = p
            .parse::<u16>()
            .map_err(|_| format!("白名单端口无效: {entry}"))?;
        (h, Some(port))
    } else {
        (entry, None)
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(PrivateTarget::Ip { ip, port });
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    if name.is_empty() {
        return Err(format!("白名单条目无效: {entry}"));
    }
    Ok(PrivateTarget::Domain { name, port })
}

/// 由 bootstrap 按配置初始化。
pub fn set_private_network_whitelist(targets: Vec<PrivateTarget>) {
    *PRIVATE_WHITELIST.write().unwrap() = targets;
}

/// 已加载的白名单条目数（启动日志用）。
pub fn private_whitelist_len() -> usize {
    PRIVATE_WHITELIST.read().unwrap().len()
}

/// 白名单为空 = 未启用收紧（`ALLOW_PRIVATE_NETWORK=false` 时也全部放行）。
fn whitelist_is_empty() -> bool {
    PRIVATE_WHITELIST.read().unwrap().is_empty()
}

/// 目标是否命中白名单（`url.host()` 枚举避免把 IPv6 字面量误判成域名）。
fn url_matches_whitelist(url: &Url) -> bool {
    let port = url.port_or_known_default();
    let entries = PRIVATE_WHITELIST.read().unwrap();
    match url.host() {
        Some(Host::Ipv4(ip)) => matches_ip(&IpAddr::V4(ip), port, &entries),
        Some(Host::Ipv6(ip)) => matches_ip(&IpAddr::V6(ip), port, &entries),
        Some(Host::Domain(domain)) => matches_domain(domain, port, &entries),
        None => false,
    }
}

/// 域名是否命中白名单（供 DNS 解析期守卫用：那里只有主机名，没有端口）。
/// 带端口约束的条目在这里只按域名匹配，端口一致性由请求前的 URL 预检保证。
fn hostname_matches_whitelist(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let entries = PRIVATE_WHITELIST.read().unwrap();
    matches_domain(&name, None, &entries)
}

fn matches_ip(ip: &IpAddr, port: Option<u16>, entries: &[PrivateTarget]) -> bool {
    entries.iter().any(|entry| match entry {
        PrivateTarget::Ip {
            ip: allowed,
            port: allowed_port,
        } => allowed == ip && port_matches(*allowed_port, port),
        PrivateTarget::Cidr { net, prefix } => ip_in_cidr(ip, net, *prefix),
        // 网段条目不约束端口：运维写网段就是要放一片，端口限制请写单 IP
        PrivateTarget::Domain { .. } => false,
    })
}

fn matches_domain(domain: &str, port: Option<u16>, entries: &[PrivateTarget]) -> bool {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    entries.iter().any(|entry| match entry {
        PrivateTarget::Domain {
            name,
            port: allowed_port,
        } => *name == domain && port_matches(*allowed_port, port),
        // IP/CIDR 条目对域名目标不生效：域名须解析后按 IP 判定，
        // 而白名单的语义是「预先信任」，解析期守卫只对未命中白名单的域名拦截
        _ => false,
    })
}

fn port_matches(allowed: Option<u16>, actual: Option<u16>) -> bool {
    match allowed {
        Some(p) => actual == Some(p),
        None => true,
    }
}

fn ip_in_cidr(ip: &IpAddr, net: &IpAddr, prefix: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => {
            let prefix = prefix.min(32);
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(*ip) & mask) == (u32::from(*net) & mask)
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            let prefix = prefix.min(128);
            let ip_seg = ip.segments();
            let net_seg = net.segments();
            let full = (prefix / 16) as usize;
            if ip_seg[..full] != net_seg[..full] {
                return false;
            }
            let rem = prefix % 16;
            if rem == 0 {
                return true;
            }
            let mask = u16::MAX << (16 - rem);
            (ip_seg[full] & mask) == (net_seg[full] & mask)
        }
        _ => false,
    }
}

/// 防护是否实际生效：`ALLOW_PRIVATE_NETWORK=false` 且白名单非空。
/// 白名单为空时按用户配置「全部放行」，等同 `true`。
fn guard_active() -> bool {
    !private_network_allowed() && !whitelist_is_empty()
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

/// 日志里的上游地址一律剥掉查询串与片段：书源站点的签名/登录态参数
/// 不该落进日志文件。解析失败时至少截掉 `?`/`#` 之后的部分。
pub fn redact_url_for_log(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(mut url) => {
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => raw.split(['?', '#']).next().unwrap_or(raw).to_string(),
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
    if !guard_active() {
        return Ok(());
    }
    // 白名单是「预先信任」：命中的目标跳过主机名与 DNS/内网判定
    if url_matches_whitelist(url) {
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
    if !guard_active() {
        return Ok(url);
    }
    if url_matches_whitelist(&url) {
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
    if !guard_active() {
        return Ok(());
    }
    if url_matches_whitelist(url) {
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

/// DNS 解析期守卫：把内网 IP 检查钉在 reqwest 的 DNS 解析环节里。
///
/// 仅靠「请求前守卫解析一次」存在 TOCTOU 窗口：守卫校验通过后，reqwest 连接时
/// 会**重新解析** DNS，控制域名解析的攻击者（恶意书源）可以让两次解析返回不同
/// 结果（DNS rebinding）。把检查放进解析环节后，连接用的就是这份已校验的结果，
/// 重定向跳也由同一解析路径覆盖。
///
/// 注意：IP 字面量不经过 DNS 解析，所以 URL 预检（[`ensure_outbound_url_allowed`]）
/// 与重定向逐跳检查仍然保留——这里是兜底，不是替代。
#[derive(Debug)]
pub struct GuardedResolver;

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            // 与 reqwest 默认的 GaiResolver 一致：端口由连接器后续填充，这里用 0 占位
            let host = name.as_str().trim_end_matches('.');
            let addrs: Vec<std::net::SocketAddr> =
                tokio::net::lookup_host((host, 0)).await?.collect();
            if guard_active() && !hostname_matches_whitelist(host) {
                check_resolved_ips(addrs.iter().map(|addr| addr.ip())).map_err(OutboundBlocked)?;
            }
            let addrs: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

/// 解析期守卫的共享句柄：不走 [`guarded_client_builder`] 的客户端
/// （需要自定义 cookie jar / 压缩等）也应装上它，否则仍有 DNS rebinding 窗口。
pub fn guarded_dns_resolver() -> std::sync::Arc<GuardedResolver> {
    std::sync::Arc::new(GuardedResolver)
}

/// 带守卫的基础 client builder：统一超时、UA、重定向策略与 DNS 解析期守卫。
pub fn guarded_client_builder(timeout: Duration, user_agent: &str) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(user_agent.to_string())
        .redirect(guarded_redirect_policy())
        .dns_resolver(guarded_dns_resolver())
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

    /// 改全局守卫状态的测试互斥：并行测试里 allow/whitelist 互相踩会偶发失败。
    static TEST_SERIALIZER: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn restore_guard_defaults() {
        set_allow_private_network(true);
        set_private_network_whitelist(Vec::new());
    }

    #[test]
    fn redirect_target_check_blocks_literal_internal() {
        let _lock = TEST_SERIALIZER.lock().unwrap();
        // 空白名单 = 全部放行，先配一条名单让防护真正生效
        set_allow_private_network(false);
        set_private_network_whitelist(parse_private_whitelist("192.168.100.0/24").unwrap());
        assert!(check_redirect_target(&Url::parse("http://127.0.0.1/").unwrap()).is_err());
        assert!(check_redirect_target(&Url::parse("http://[::1]/").unwrap()).is_err());
        assert!(check_redirect_target(&Url::parse("http://localhost/").unwrap()).is_err());
        // 白名单命中的目标放行（字面 IP，无需 DNS）
        assert!(check_redirect_target(&Url::parse("http://192.168.100.99:9999/").unwrap()).is_ok());
        // 守护状态下 localhost 既命中主机名黑名单，也过不了 DNS 解析检查
        set_allow_private_network(true);
        assert!(check_redirect_target(&Url::parse("http://127.0.0.1/").unwrap()).is_ok());
        restore_guard_defaults();
    }

    #[test]
    fn whitelist_parsing_and_matching() {
        let _lock = TEST_SERIALIZER.lock().unwrap();
        set_private_network_whitelist(
            parse_private_whitelist(
                "192.168.100.99:9999, 192.168.100.0/24, [::1]:8080, NAS.Lan, 10.0.0.1",
            )
            .unwrap(),
        );
        let entries = PRIVATE_WHITELIST.read().unwrap().clone();
        assert_eq!(entries.len(), 5);
        assert_eq!(
            entries[0],
            PrivateTarget::Ip {
                ip: "192.168.100.99".parse().unwrap(),
                port: Some(9999)
            }
        );
        assert_eq!(
            entries[1],
            PrivateTarget::Cidr {
                net: "192.168.100.0".parse().unwrap(),
                prefix: 24
            }
        );
        assert_eq!(
            entries[2],
            PrivateTarget::Ip {
                ip: "::1".parse().unwrap(),
                port: Some(8080)
            }
        );
        assert_eq!(
            entries[3],
            PrivateTarget::Domain {
                name: "nas.lan".to_string(),
                port: None
            }
        );

        // 域名匹配大小写与尾点不敏感；端口约束一致才算命中
        assert!(url_matches_whitelist(
            &Url::parse("http://nas.lan/x").unwrap()
        ));
        assert!(url_matches_whitelist(
            &Url::parse("http://NAS.LAN.:80/x").unwrap()
        ));
        assert!(!url_matches_whitelist(
            &Url::parse("http://other.lan/x").unwrap()
        ));
        // IP 目标：单 IP 带端口要端口一致；网段条目不看端口
        assert!(url_matches_whitelist(
            &Url::parse("http://192.168.100.99:9999/").unwrap()
        ));
        assert!(url_matches_whitelist(
            &Url::parse("http://192.168.100.123/").unwrap()
        ));
        assert!(url_matches_whitelist(
            &Url::parse("http://10.0.0.1:1/").unwrap()
        ));
        assert!(!url_matches_whitelist(
            &Url::parse("http://10.0.0.2/").unwrap()
        ));
        assert!(url_matches_whitelist(
            &Url::parse("http://[::1]:8080/").unwrap()
        ));
        assert!(!url_matches_whitelist(
            &Url::parse("http://[::1]/").unwrap()
        ));
        // 裸 IPv6 白名单条目（无端口）匹配任意端口
        set_private_network_whitelist(parse_private_whitelist("::1, fd00::1/8").unwrap());
        assert!(url_matches_whitelist(
            &Url::parse("http://[::1]:1/").unwrap()
        ));
        assert!(url_matches_whitelist(
            &Url::parse("http://[fd00::1]/").unwrap()
        ));
        assert!(!url_matches_whitelist(
            &Url::parse("http://[fe00::1]/").unwrap()
        ));
        // 公网地址不在名单里
        assert!(!url_matches_whitelist(
            &Url::parse("http://1.1.1.1/").unwrap()
        ));
        restore_guard_defaults();
    }

    #[test]
    fn whitelist_rejects_invalid_entries() {
        assert!(parse_private_whitelist("192.168.0.0/33").is_err());
        assert!(parse_private_whitelist("192.168.0.0/notaprefix").is_err());
        assert!(parse_private_whitelist("[::1").is_err());
        assert!(parse_private_whitelist("foo:bar").is_err());
    }

    #[test]
    fn guard_with_whitelist_blocks_unlisted_and_allows_listed() {
        let _lock = TEST_SERIALIZER.lock().unwrap();
        set_allow_private_network(false);
        set_private_network_whitelist(
            parse_private_whitelist("192.168.100.99:9999, nas.lan").unwrap(),
        );
        // 名单内：字面 IP 直接放行，域名跳过 DNS 检查
        assert!(
            ensure_outbound_url_str_allowed_blocking("http://192.168.100.99:9999/content").is_ok()
        );
        assert!(ensure_outbound_url_str_allowed_blocking("http://nas.lan/api").is_ok());
        // 名单外：私网字面量、内网主机名一律拦截
        assert!(ensure_outbound_url_str_allowed_blocking("http://192.168.100.98/").is_err());
        assert!(ensure_outbound_url_str_allowed_blocking("http://10.1.2.3/").is_err());
        assert!(ensure_outbound_url_str_allowed_blocking("http://localhost:6379/").is_err());
        restore_guard_defaults();
    }

    #[test]
    fn empty_whitelist_with_allow_false_allows_everything() {
        let _lock = TEST_SERIALIZER.lock().unwrap();
        set_allow_private_network(false);
        set_private_network_whitelist(Vec::new());
        // 名单为空 = 全部放行（等同 allow=true），不做 DNS 检查直接过
        assert!(ensure_outbound_url_str_allowed_blocking("http://127.0.0.1/").is_ok());
        assert!(ensure_outbound_url_str_allowed_blocking("http://192.168.1.1:9999/").is_ok());
        restore_guard_defaults();
    }
}
