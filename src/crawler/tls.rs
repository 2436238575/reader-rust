//! TLS 栈装配：reqwest 0.13 移除了特性级的 provider/根证书组合
//! （`rustls` 特性固定 aws-lc-rs provider + 平台根证书，aws-lc-rs 的 C 代码
//! 需要 cmake，musl 静态镜像不满足；`rustls-no-provider` 则把装配交给代码），
//! 这里显式固定回 0.12 时代 `rustls-tls` 的等价组合：
//! **ring provider + webpki-roots 捆绑根证书**，全站出站共用。
//!
//! 两个入口：
//! - [`shared_client_config`]：经 `ClientBuilder::tls_backend_preconfigured`
//!   注入各 reqwest client（爬虫、JS 引擎、AI 代理），根证书与 provider 都确定；
//! - [`ensure_default_crypto_provider`]：把 ring 安装为进程级默认 provider。
//!   rustls-no-provider 下，任何未预配置的 `Client::new()` / `builder().build()`
//!   都会在构建时查找进程默认 provider，找不到直接 panic——应用入口
//!   （`app::bootstrap::build_state`）与共享配置构建都会先装它兜底。

use std::sync::{Arc, OnceLock};

/// 把 ring 安装为进程级默认 CryptoProvider（幂等：已安装时忽略错误返回）。
///
/// 只保证「没预配置的 reqwest client 不会 panic」；正式出站链路一律走
/// [`shared_client_config`] 预配置，不依赖进程默认。
pub fn ensure_default_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// 共享 rustls 客户端配置：ring provider + webpki-roots 捆绑根证书。
///
/// 进程内只构建一次（内部是 Arc 共享，克隆零拷贝）。ALPN 协议列表留空，
/// 由 reqwest/hyper-rustls 按启用特性填充（当前特性集下即 HTTP/1.1）。
pub fn shared_client_config() -> rustls::ClientConfig {
    static CONFIG: OnceLock<rustls::ClientConfig> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            // 兜底：任何出站 client 构建前，进程默认 provider 一定已就位
            ensure_default_crypto_provider();
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("ring provider 支持 rustls 默认协议版本（TLS1.2/1.3）")
            .with_root_certificates(roots)
            .with_no_client_auth()
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_config_is_reused_and_provider_installed() {
        let a = shared_client_config();
        let b = shared_client_config();
        assert_eq!(a.alpn_protocols, b.alpn_protocols);
        // 构建共享配置后进程默认 provider 必须已就位（未预配置的 client 不 panic）
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
