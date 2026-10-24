//! 客户端 IP 解析：仅当对端 socket 地址命中受信代理（TRUSTED_PROXIES）时
//! 才解析 `X-Forwarded-For`。
//!
//! 生产部署通常挂在 nginx/Caddy/Docker NAT 后面，所有请求的对端地址都是
//! 代理 IP；登录限速若按 socket 地址记账，攻击者错 5 次会把全体访问者封
//! 6 小时。`TRUSTED_PROXIES` 声明「允许直连本机的可信代理」（IP 或 CIDR），
//! 只有对端命中受信列表时才从 XFF 还原真实客户端地址——绝不无条件信任
//! XFF：任何客户端都能伪造该头，直连场景照抄 XFF 等于把限速的记账权交给
//! 攻击者。
//!
//! 条目解析口径与 `crawler::url_guard` 的 `PRIVATE_NETWORK_WHITELIST` 一致
//! （IP / CIDR、裸 IPv6 字面量），但这里只认 IP/CIDR：XFF 里不出现域名与
//! 端口，代理本身就是地址。

use http::HeaderMap;
use std::net::IpAddr;

/// 受信代理条目：单个 IP 或 CIDR 网段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustedProxy {
    Ip(IpAddr),
    Cidr { net: IpAddr, prefix: u8 },
}

/// 解析 TRUSTED_PROXIES 配置串（逗号分隔）。
///
/// 条目形态：`172.17.0.1`、`172.17.0.0/16`、`::1`、`fd00::/8`。
/// 解析失败的条目直接报错——静默忽略会让「收紧」悄悄变成「按代理 IP 记账」
/// 而无从察觉（与 PRIVATE_NETWORK_WHITELIST 同一原则）。
pub fn parse_trusted_proxies(raw: &str) -> Result<Vec<TrustedProxy>, String> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let entry = part.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((addr, prefix)) = entry.rsplit_once('/') {
            let ip: IpAddr = addr
                .parse()
                .map_err(|_| format!("受信代理网段无效: {entry}"))?;
            let max = match ip {
                IpAddr::V4(_) => 32,
                IpAddr::V6(_) => 128,
            };
            let prefix: u8 = prefix
                .parse()
                .map_err(|_| format!("受信代理网段前缀无效: {entry}"))?;
            if prefix > max {
                return Err(format!("受信代理网段前缀超出范围: {entry}"));
            }
            out.push(TrustedProxy::Cidr { net: ip, prefix });
        } else {
            let ip: IpAddr = entry
                .parse()
                .map_err(|_| format!("受信代理条目无效: {entry}（只支持 IP / CIDR）"))?;
            out.push(TrustedProxy::Ip(ip));
        }
    }
    Ok(out)
}

/// 解析配置串；解析失败（启动时 load() 已告警并清空）回退空列表，
/// 即只按 socket 地址记账——最保守的行为。
pub fn parse_or_default(raw: &str) -> Vec<TrustedProxy> {
    parse_trusted_proxies(raw).unwrap_or_default()
}

/// 解析记账用的客户端 IP。
///
/// 规则：
/// 1. 受信列表为空，或对端 socket 地址不在受信列表里 → 一律用 socket 地址
///    （忽略 XFF，可伪造的头不参与记账）。
/// 2. 对端受信 → 汇总全部 `X-Forwarded-For`（可能多条头、每条又逗号分隔），
///    **从右往左**找第一个不在受信列表里的地址：右端是离我们最近的代理，
///    左端才是发起者。
/// 3. 任一条目格式非法、或全部受信（无从还原）→ 回退 socket 地址：
///    宁可按代理记账，也不采信更左端可被伪造的条目。
pub fn resolve_client_ip(
    socket_ip: IpAddr,
    headers: &HeaderMap,
    trusted: &[TrustedProxy],
) -> String {
    if trusted.is_empty() || !ip_matches(socket_ip, trusted) {
        return socket_ip.to_string();
    }
    let mut entries: Vec<&str> = Vec::new();
    for value in headers.get_all("X-Forwarded-For") {
        let Ok(text) = value.to_str() else {
            return socket_ip.to_string();
        };
        entries.extend(text.split(','));
    }
    for entry in entries.iter().rev() {
        let Ok(ip) = entry.trim().parse::<IpAddr>() else {
            return socket_ip.to_string();
        };
        if !ip_matches(ip, trusted) {
            return ip.to_string();
        }
    }
    socket_ip.to_string()
}

fn ip_matches(ip: IpAddr, trusted: &[TrustedProxy]) -> bool {
    trusted.iter().any(|entry| match entry {
        TrustedProxy::Ip(allowed) => *allowed == ip,
        TrustedProxy::Cidr { net, prefix } => ip_in_cidr(&ip, net, *prefix),
    })
}

/// IPv4/IPv6 CIDR 判断（与 `crawler::url_guard::ip_in_cidr` 同一口径；
/// 该函数是 url_guard 私有实现，util 作为底座层不能反向依赖 crawler）。
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

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn proxies(list: &[TrustedProxy]) -> Vec<TrustedProxy> {
        list.to_vec()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            let name =
                http::header::HeaderName::from_bytes(name.as_bytes()).expect("测试头名必须合法");
            map.insert(
                name,
                HeaderValue::from_str(value).expect("测试头值必须合法"),
            );
        }
        map
    }

    fn v4(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn parse_accepts_ip_and_cidr_for_both_families() {
        assert_eq!(
            parse_trusted_proxies("172.17.0.1, 10.0.0.0/8, ::1,fd00::/8"),
            Ok(vec![
                TrustedProxy::Ip(v4("172.17.0.1")),
                TrustedProxy::Cidr {
                    net: v4("10.0.0.0"),
                    prefix: 8
                },
                TrustedProxy::Ip(v4("::1")),
                TrustedProxy::Cidr {
                    net: v4("fd00::"),
                    prefix: 8
                },
            ])
        );
        // 空串 / 空白条目 → 空列表（不信任任何代理）
        assert_eq!(parse_trusted_proxies(""), Ok(Vec::new()));
        assert_eq!(parse_trusted_proxies(" , "), Ok(Vec::new()));
    }

    #[test]
    fn parse_rejects_invalid_entries() {
        assert!(parse_trusted_proxies("not-an-ip").is_err());
        assert!(
            parse_trusted_proxies("nas.lan").is_err(),
            "域名不是代理地址"
        );
        assert!(parse_trusted_proxies("10.0.0.0/99").is_err());
        assert!(parse_trusted_proxies("::1/129").is_err());
        // 解析失败回退空列表（按直连记账）
        assert!(parse_or_default("junk").is_empty());
    }

    #[test]
    fn untrusted_socket_ignores_xff_entirely() {
        let trusted = proxies(&[TrustedProxy::Cidr {
            net: v4("10.0.0.0"),
            prefix: 8,
        }]);
        let ip = resolve_client_ip(
            v4("203.0.113.9"),
            &headers(&[("X-Forwarded-For", "1.2.3.4")]),
            &trusted,
        );
        assert_eq!(ip, "203.0.113.9", "直连不在受信列表时必须忽略 XFF");
    }

    #[test]
    fn trusted_proxy_takes_client_from_xff() {
        let trusted = proxies(&[TrustedProxy::Cidr {
            net: v4("10.0.0.0"),
            prefix: 8,
        }]);
        let ip = resolve_client_ip(
            v4("10.0.0.5"),
            &headers(&[("X-Forwarded-For", "1.2.3.4")]),
            &trusted,
        );
        assert_eq!(ip, "1.2.3.4");
    }

    #[test]
    fn rightmost_untrusted_entry_wins() {
        let trusted = proxies(&[TrustedProxy::Cidr {
            net: v4("10.0.0.0"),
            prefix: 8,
        }]);
        let ip = resolve_client_ip(
            v4("10.0.0.5"),
            &headers(&[("X-Forwarded-For", "1.2.3.4, 10.0.0.5")]),
            &trusted,
        );
        assert_eq!(ip, "1.2.3.4", "10.0.0.5 受信，取其左侧的 1.2.3.4");
    }

    #[test]
    fn invalid_xff_falls_back_to_socket_ip() {
        let trusted = proxies(&[TrustedProxy::Ip(v4("10.0.0.5"))]);
        let ip = resolve_client_ip(
            v4("10.0.0.5"),
            &headers(&[("X-Forwarded-For", "not-an-ip")]),
            &trusted,
        );
        assert_eq!(ip, "10.0.0.5");
        // 链中间混进非法条目：整条不可信，回退
        let ip = resolve_client_ip(
            v4("10.0.0.5"),
            &headers(&[("X-Forwarded-For", "1.2.3.4, garbage, 10.0.0.5")]),
            &trusted,
        );
        assert_eq!(ip, "10.0.0.5");
    }

    #[test]
    fn empty_trusted_list_always_uses_socket_ip() {
        let ip = resolve_client_ip(
            v4("10.0.0.5"),
            &headers(&[("X-Forwarded-For", "1.2.3.4")]),
            &[],
        );
        assert_eq!(ip, "10.0.0.5", "空列表永远用 socket 地址");
    }

    #[test]
    fn all_trusted_or_missing_xff_falls_back_to_socket_ip() {
        let trusted = proxies(&[
            TrustedProxy::Ip(v4("10.0.0.5")),
            TrustedProxy::Ip(v4("10.0.0.6")),
        ]);
        // 全部受信：无从还原
        let ip = resolve_client_ip(
            v4("10.0.0.5"),
            &headers(&[("X-Forwarded-For", "10.0.0.6, 10.0.0.5")]),
            &trusted,
        );
        assert_eq!(ip, "10.0.0.5");
        // 对端受信但没有 XFF 头
        let ip = resolve_client_ip(v4("10.0.0.5"), &HeaderMap::new(), &trusted);
        assert_eq!(ip, "10.0.0.5");
    }

    #[test]
    fn multiple_xff_headers_are_joined_before_scanning() {
        let trusted = proxies(&[TrustedProxy::Cidr {
            net: v4("10.0.0.0"),
            prefix: 8,
        }]);
        // 两条头各带一段链：等价于 "1.2.3.4, 10.0.0.9, 198.51.100.7, 10.0.0.5"
        let mut map = HeaderMap::new();
        map.append(
            "X-Forwarded-For",
            HeaderValue::from_static("1.2.3.4, 10.0.0.9"),
        );
        map.append(
            "X-Forwarded-For",
            HeaderValue::from_static("198.51.100.7, 10.0.0.5"),
        );
        let ip = resolve_client_ip(v4("10.0.0.5"), &map, &trusted);
        assert_eq!(ip, "198.51.100.7", "从最右非受信处截断");
    }

    #[test]
    fn ipv6_chain_resolves() {
        let trusted = proxies(&[TrustedProxy::Ip(v4("::1"))]);
        let ip = resolve_client_ip(
            v4("::1"),
            &headers(&[("X-Forwarded-For", "2001:db8::7, ::1")]),
            &trusted,
        );
        assert_eq!(ip, "2001:db8::7");
    }
}
