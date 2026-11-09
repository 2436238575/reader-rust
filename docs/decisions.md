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

## 2026-11-09 审阅返工（P0 第一轮）

- **`deleteBookCache` / `purgeCache scope=book` / 移出书架不再删除章节登记行**：登记行是 `(bookId, index)` 的解析载体，删掉它离线解析就断了，而留下的反而是可再生数据，方向相反。改为只重置 `book_toc_state`（下次读目录照常重抓，效果等同「清目录」）。整表删除只剩换源（新源换 index 空间）与 refresh=1（删后立即重抓）两个场景。审阅建议的「删正文版本」**不采纳**：正文历史严格保留是项目级不变量（编辑章/导入版本在其中）。
- **`/v1` 错误体的 `traceId` 由响应中间件统一补**：只改写 problem+json 的错误响应，handler 无需关心；`require_auth` 的手工盖章随之删除。
- **`/v1` 挂入站超时与并发层**，并发许可与 `/reader3` 受保护组共享同一信号量（全局预算不翻倍）。
- **新增 `V1User` 提取器**（rejection = `Problem`）：即使某条 /v1 路由漏挂鉴权中间件，失败形态也不会漏出 `ApiResponse`。
- **`/v1/me` 账号行不存在改用 `AUTH_REFRESH_REPLAYED`**：`AUTH_EXPIRED` 会引导前端静默刷新，而刷新救不回不存在的账号，会形成一轮无效循环；`AUTH_REFRESH_REPLAYED` 的前端语义正是「必须重新登录」。
- **状态码对齐契约**：`VALIDATION_FAILED` / `INVALID_CURSOR` / `UNKNOWN_FIELD` → 422（语法合法但值非法）；`MALFORMED_REQUEST` / `UPSTREAM_BLOCKED` 保持 400。
- **spec 的 problem 可选成员加 `nullable = false`**：实现是缺省省略而不是 null，生成的 schema 从 `string | null` 收敛为可选属性，openapi-typescript 不再产出三态。
- **生成 yaml 定位为 as-built，不另存 53 路径的目标文件**：spec-first 的闸门落在契约文档（Draft-2 端点表）+ 决策记录上，生成 yaml 随实现增长；避免两份 yaml 双真相。
- **本地书（TXT/EPUB）的 index 解析**：v1 内容端点落地时按「本地书直接经本地服务按 index 取章」分支处理，不写入 chapters 表（本地目录由文件确定性生成，逐次解析成本低，写表无收益）。已注记到 goal 的 P1。

## 2026-11-09 审阅第二轮

- **入站超时中间件参数化（`TimeoutShape::Legacy | Problem`）**：`/reader3` 保持 408 + ApiResponse；`/v1` 回 `504 PROCESSING_TIMEOUT` problem+json（408 的语义是客户端发请求太慢）。traceId 在中间件内直接盖章（超时响应不经过内层 trace 中间件）。
- **新增 `SESSION_INVALID`（401）**：账号行不存在这类「会话已失效、刷新救不回」的状态不再借用 `AUTH_REFRESH_REPLAYED`——那是安全告警语义（重放检测、家族吊销），前端按码埋点会跑偏。兼容变更。
- **`deleteBookCache` 的产品语义留待 P4 拆分**：登记行保留后，书籍级清理真正能释放的空间只剩候选清单与详情文件。P4 把「清理本书缓存」拆成两个动作——「重置目录/候选」（快）与「清除已下载正文」（真释放空间；实现约束：编辑/导入版本必须保留，仅删可再生上游版本并重置版本指针，图片连带）。未拍板，先记约束。

## 2026-11-09 auth 域（后端批）

- **响应体保留 `accessToken`**：契约 §6.1 只写「不再返回 refreshToken」，但 §6.2 允许 Bearer 客户端——拿不到 JWT 就无法使用。保留 `accessToken` + `accessTokenExpiresAt`；`refreshToken` 永不进 body。
- **refresh Cookie Path 由 `COOKIE_PATH` 推导**：根部署 `/reader3` → `/v1/auth`；子路径部署 `/read/reader3` → `/read/v1/auth`；非标准配置回落 `/v1/auth`。access Cookie 一律 `Path=/`（两套命名空间共用同一会话）。
- **`SameSite` 全局切 Lax**：与契约一致。Strict 时「从外部应用点链接进入」的场景收不到 Cookie；跨站非 GET 已由 Lax 挡掉，自定义头检查给 `/v1` 补第二道。
- **登录限速错误变体 `AppError::TooManyRequests { message, retry_after_secs }`**：限速从「BadRequest 文案可辩」变成结构化变体，`/reader3` 顺带从 400 变 429（前端按 errorMsg 展示，无感）；`Problem` 带 `retry_after` 进 Retry-After 头。
- **刷新失败细分由 service 暴露**（`refresh_session_detailed` → `RefreshFailure::Invalid | Replayed`）：`/v1/auth/refresh` 据此区分 `AUTH_EXPIRED` 与 `AUTH_REFRESH_REPLAYED`；`/reader3` 语义不变（统一 401 + 清 Cookie）。
- **CSRF 检查挂 `/v1` 全域（公开端点也在内），Bearer 豁免**：`X-Requested-With: XMLHttpRequest` 缺失即 403；登录接口同样要求（防 Login CSRF）。`/reader3` 过渡期不挂（等域切换收尾收全局）。
- **宽限窗口的测试手法**：refresh 重放宽限是 30s 常量，集成测试用同一 SQL 回拨 `used_at` 穿过窗口（与该服务单测一致），不为此加配置项。
- **auth 域拆两批交付**：本批是后端增量（/reader3 认证原样保留，双轨并存）。下一批前端切换 + 删除 /reader3/login、refreshToken、logout、changePassword + auth_flow 移植 + e2e 核对。SSE 票据端点延到 P2（消费方 /v1/tasks/{id}/events 还没落地，先建是死代码）。
