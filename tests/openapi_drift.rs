//! `openapi.yaml` 漂移检查：仓库根目录的锚文件必须与代码生成的文档一致。
//!
//! 这是「契约即代码」的 CI 闸门——实现改了 schema/端点而不更新锚文件，
//! 合并直接失败。更新锚文件：
//!
//! ```bash
//! UPDATE_OPENAPI=1 cargo test --test openapi_drift
//! ```

use std::path::Path;
use std::sync::OnceLock;

fn generated_yaml() -> &'static str {
    static YAML: OnceLock<String> = OnceLock::new();
    YAML.get_or_init(reader_rust::api::v1::openapi_yaml)
}

#[test]
fn openapi_yaml_matches_code() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi.yaml");
    if std::env::var("UPDATE_OPENAPI").is_ok() {
        std::fs::write(&path, generated_yaml()).expect("write openapi.yaml");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        committed,
        generated_yaml(),
        "openapi.yaml 与代码生成结果不一致——运行 `UPDATE_OPENAPI=1 cargo test --test openapi_drift` 更新锚文件"
    );
}

/// 已交付端点的清单断言：新增域时（如 auth→me→sources）在这里补路径。
/// 这比「路径总数」稳——漏注册 path 时生成物与锚文件会同样缺失，
/// 漂移检查天然抓不到，只有清单断言能兜住（2026-11-09 审阅发现的问题）。
const REQUIRED_PATHS: &[&str] = &[
    "/v1/me:",
    "/v1/sources:",
    "/v1/auth/login:",
    "/v1/auth/refresh:",
    "/v1/auth/logout:",
    "/v1/auth/password:",
];

#[test]
fn every_delivered_endpoint_is_registered_in_the_doc() {
    let yaml = generated_yaml();
    for path in REQUIRED_PATHS {
        assert!(
            yaml.contains(path),
            "OpenAPI 缺少已交付端点 {path}——检查 V1ApiDoc 的 paths(...) 注册"
        );
    }
}

/// 生成的文档必须是合法 OpenAPI：版本字段、paths 与 components 齐全。
/// 漂移检查之外的最小自检，不依赖外部校验器。
#[test]
fn generated_doc_has_minimum_shape() {
    let yaml = generated_yaml();
    assert!(yaml.contains("openapi:"), "缺少 openapi 版本字段");
    assert!(yaml.contains("  /v1/me:"), "缺少 /v1/me 路径");
    assert!(yaml.contains("components:"), "缺少 components");
}
