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

/// 生成的文档必须是合法 OpenAPI：版本字段、paths 与 components 齐全。
/// 漂移检查之外的最小自检，不依赖外部校验器。
#[test]
fn generated_doc_has_minimum_shape() {
    let yaml = generated_yaml();
    assert!(yaml.contains("openapi:"), "缺少 openapi 版本字段");
    assert!(yaml.contains("  /v1/me:"), "缺少 /v1/me 路径");
    assert!(yaml.contains("components:"), "缺少 components");
}
