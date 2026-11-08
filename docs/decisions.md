# 自定决策记录

goal 执行中遇到「决策表与 spec 都沉默」的问题时，按 §5.4 的默认规则自行决定的记录。

## 2026-11-09 P0 垂直切片

- **`type` 字段取 `about:blank`**：RFC 9457 的缺省类型。错误语义由 `code` 承载（前端只 switch code），不为每个码维护错误文档页；将来要给单个码补文档时再覆盖 `type`。
- **新增通用兜底码 `NOT_FOUND` / `CONFLICT`**：`AppError` → `Problem` 的兜底映射无法知道具体资源。领域 handler 落地时必须用带资源的精确码（`BOOK_NOT_FOUND` 等），兜底码只服务未预期路径。
- **problem 的可选成员缺省省略**：`instance` / `traceId` / `upstreamCode` / `errors` 不出现而不是 `null`。RFC 9457 惯例，且前端不依赖成员存在性；「字段始终出现」约束只作用于资源 schema。
- **`INTERNAL` 的 detail 用 `AppError::client_message()`**：与 `/reader3` 同一份脱敏口径，细节只进日志。
- **OpenAPI 3.1 由 utoipa 6 原生输出**：工具链定稿为 utoipa 6（`yaml` feature）+ openapi-typescript 7；生成锚文件在仓库根 `openapi.yaml`，漂移检查 `cargo test --test openapi_drift`（更新：`UPDATE_OPENAPI=1`）。
- **`/v1/me` 的 `createdAt` 按 users 表毫秒口径换算**，输出 RFC 3339 秒级字符串（契约 §5.2「秒级足够」）；库内 users 时间戳与 DDL 注释不符（注释写秒、实写毫秒），以代码为准。
- **限定令牌（scope）在 `/v1` 一律 403 `SCOPE_FORBIDDEN`**：限定令牌只服务 `/reader3` 的书源代理路径白名单，v1 不接受。
