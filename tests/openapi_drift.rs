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
///
/// 条目写**完整路径**（不带尾冒号）。断言按 YAML 里 `paths` 的两空格缩进
/// 键整行精确匹配：`contains("/…/content:")` 会把 `/…/content:select:` 也
/// 算命中，一个端点消失能被另一个顶包（2026-11-12 审阅发现）。
const REQUIRED_PATHS: &[&str] = &[
    // auth
    "/v1/auth/login",
    "/v1/auth/refresh",
    "/v1/auth/logout",
    "/v1/auth/password",
    // me / shelf
    "/v1/me",
    "/v1/me/shelf",
    "/v1/me/shelf/{bookId}",
    "/v1/me/shelf:batchDelete",
    "/v1/me/shelf:order",
    "/v1/me/marks",
    "/v1/me/marks:batch",
    "/v1/me/marks:batchDelete",
    "/v1/me/userdata:batch",
    "/v1/me/groups",
    "/v1/me/groups:order",
    "/v1/me/groups/{groupId}",
    "/v1/me/replace-rules",
    "/v1/me/replace-rules/{name}",
    "/v1/me/replace-rules:batch",
    "/v1/me/replace-rules:batchDelete",
    // cache
    "/v1/cache",
    "/v1/cache/stats",
    // books / chapters
    "/v1/books/{bookId}",
    "/v1/books/{bookId}/cache",
    "/v1/books/{bookId}/chapter-marks",
    "/v1/books/{bookId}/chapters",
    "/v1/books/{bookId}/cache-search",
    "/v1/books/{bookId}/source-candidates/events",
    "/v1/books/{bookId}/chapters:batchContent",
    "/v1/books/{bookId}/chapters:preview-import",
    "/v1/books/{bookId}/chapters:import",
    "/v1/books/{bookId}/chapters:upload-import-chunk",
    "/v1/books/{bookId}/chapters/import-diff",
    "/v1/books/{bookId}/chapters/{index}",
    "/v1/books/{bookId}/chapters/{index}/title",
    "/v1/books/{bookId}/chapters/{index}/content",
    "/v1/books/{bookId}/chapters/{index}/content/edit",
    "/v1/books/{bookId}/chapters/{index}/content:select",
    "/v1/books/{bookId}/chapters/{index}/content:import",
    "/v1/books/{bookId}/chapters/{index}/versions",
    "/v1/books/{bookId}/chapters/{index}/versions/{versionId}",
    "/v1/books/{bookId}/chapters/{index}/versions/{versionId}/diff",
    "/v1/books/{bookId}/chapters/{index}/comments",
    "/v1/books/{bookId}/chapters/{index}/comments/index",
    "/v1/books/{bookId}/chapters/{index}/images",
    "/v1/books/{bookId}/content:reset",
    // sources / explore / local books
    "/v1/sources",
    "/v1/sources/{id}",
    "/v1/sources/{sourceId}/explore",
    "/v1/sources:batchUpsert",
    "/v1/sources:batchDelete",
    "/v1/sources:deleteInvalid",
    "/v1/sources:test",
    "/v1/sources:import",
    "/v1/sources/{sourceId}:test",
    "/v1/local-books",
    // tasks / searches / SSE tickets（P2）
    "/v1/searches",
    "/v1/tasks",
    "/v1/tasks/{taskId}",
    "/v1/tasks/{taskId}/result",
    "/v1/tasks/{taskId}/events",
    "/v1/sse-tickets",
    // 服务器文件浏览 / 备份恢复（协议挂载 /v1/webdav 不在 OpenAPI 内）
    "/v1/me/files",
    "/v1/me/files/{path}",
    "/v1/me/files:batchDelete",
    "/v1/me/backups",
    "/v1/me/backups:inspect",
    "/v1/me/backups:restore",
];

/// YAML 里 `paths` 的键固定是两空格缩进、以冒号结尾的一行。逐行精确
/// 比较而不是 `contains`：路径之间存在前缀关系（`…/content` 与
/// `…/content:select`），宽松匹配会让两个端点互相顶包。
fn has_path(yaml: &str, path: &str) -> bool {
    let expected = format!("  {path}:");
    yaml.lines().any(|line| line == expected)
}

#[test]
fn every_delivered_endpoint_is_registered_in_the_doc() {
    let yaml = generated_yaml();
    for path in REQUIRED_PATHS {
        assert!(
            has_path(yaml, path),
            "OpenAPI 缺少已交付端点 {path}——检查 V1ApiDoc 的 paths(...) 注册"
        );
    }
}

/// 同名 schema 会被 utoipa 后注册者覆盖：WebDAV 批删曾用
/// `BatchDeleteRequest`，把替换规则批删的 `names` 静默改成了 `paths`。
/// 两个端点必须各自引用自己的 schema。
#[test]
fn batch_delete_schemas_do_not_collide() {
    let yaml = generated_yaml();
    let rules_schema = yaml
        .split("    BatchDeleteRequest:\n")
        .nth(1)
        .expect("替换规则 BatchDeleteRequest schema 存在");
    assert!(
        rules_schema.contains("- names"),
        "BatchDeleteRequest 必须保留 names 字段"
    );
    let files_schema = yaml
        .split("    WebdavBatchDeleteRequest:\n")
        .nth(1)
        .expect("WebDAV WebdavBatchDeleteRequest schema 存在");
    assert!(
        files_schema.contains("- paths"),
        "WebdavBatchDeleteRequest 必须有 paths 字段"
    );
    let rules_path = yaml
        .split("  /v1/me/replace-rules:batchDelete:\n")
        .nth(1)
        .expect("替换规则批删路径存在");
    assert!(rules_path.contains("'#/components/schemas/BatchDeleteRequest'"));
    let files_path = yaml
        .split("  /v1/me/files:batchDelete:\n")
        .nth(1)
        .expect("WebDAV 批删路径存在");
    assert!(files_path.contains("'#/components/schemas/WebdavBatchDeleteRequest'"));
}

/// 查询参数必须是 `in: query`：`IntoParams` 默认生成 Path，`/v1/tasks` 的
/// `state`/`cursor`/`limit` 曾因此被写成 path 参数（路径模板里根本没有它们），
/// 前端生成的类型不可用。这里按路径段定点断言，防止再次回归。
#[test]
fn task_query_params_are_declared_as_query() {
    let yaml = generated_yaml();
    let tasks = yaml
        .split("  /v1/tasks:\n")
        .nth(1)
        .expect("/v1/tasks 段存在");
    for name in ["state", "cursor", "limit"] {
        let needle = format!("- name: {name}\n        in: query");
        assert!(tasks.contains(&needle), "{name} 必须是 in: query");
    }
    let result = yaml
        .split("  /v1/tasks/{taskId}/result:\n")
        .nth(1)
        .expect("result 段存在");
    for name in ["cursor", "limit"] {
        let needle = format!("- name: {name}\n        in: query");
        assert!(result.contains(&needle), "result.{name} 必须是 in: query");
    }
}

/// 请求体的必填字段必须进入 schema：`q`/`path` 用 `Option<String>` 是为了
/// 运行时返回 422 + JSON Pointer，若忘了 `#[schema(required = true)]`，
/// 生成客户端会把必填字段当成可选。
#[test]
fn task_request_required_fields_are_in_spec() {
    let yaml = generated_yaml();
    let search = yaml
        .split("    SearchCreateRequest:\n")
        .nth(1)
        .expect("SearchCreateRequest schema 存在");
    assert!(search.contains("required:\n      - q"), "q 必须是必填");
    let ticket = yaml
        .split("    SseTicketRequest:\n")
        .nth(1)
        .expect("SseTicketRequest schema 存在");
    assert!(
        ticket.contains("required:\n      - path"),
        "path 必须是必填"
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
