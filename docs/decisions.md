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
- **`/v1/me` 账号行不存在改用 `AUTH_REFRESH_REPLAYED`**（**已被下文「审阅第二轮」的 `SESSION_INVALID` 覆盖，勿照此实现**）：当时的考虑是「必须重新登录」的前端语义；后续审阅指出该码是安全告警语义，已改用 `SessionInvalid`。
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

## 2026-11-09 auth 域收尾（前端切换 + /reader3 认证下线)

- **`SessionTokens` 带 `username`**：登录响应加一个 `username` 字段（契约只列了 accessTokenExpiresAt）——前端 hint 与登录页展示都需要它，且旧 `/reader3` 登录响应本就带用户名，加性字段零破坏。
- **`auth_flow.rs` 删除而非移植**：其覆盖的登录/刷新/重放/登出/改密语义已由 `v1_auth_flow.rs` 的九条用例完整覆盖（后者按 v1 契约断言），逐条移植等于重复维护。删除理由写进提交信息。
- **测试脚手架统一走 v1**：`tests/common::login` 与十四个集成测试的本地登录助手一并切到 `/v1/auth/login`（带 CSRF 头、解析裸载荷）；e2e 的 fixture 就绪探测同口径。
- **`getUserInfo` 过渡期留在 `/reader3`**：会话 Cookie 两套命名空间共用，me 域（用户资料 + stats/settings）切换时一并迁移到 `/v1/me`。
- **存量浏览器会话需重新登录一次**：refresh Cookie `Path` 收紧到 `/v1/auth`，旧 Cookie（Path=/reader3 或 /）不再被刷新端点读取。

## 2026-11-09 sources 读链路（列表）+ me 资料切换

- **本批只迁 `/v1/sources` 列表**：单源详情与写链路（save/test/delete/import）随 sources 写批次一起迁移——编辑器拿不到 v1 写接口前没有迁移价值。列表期间两套读路径读同一张表，状态一致；前端列表页已切 v1，编辑器继续走 `/reader3`。
- **列表排序固定为 `id` 升序**：`updated_at` 随编辑变化，做 keyset 游标会漏行/重行；`id` 不可变。展示排序（customOrder/weight）由客户端处理。游标载荷是 `{id}` 的 base64url，解码失败 → 422 `INVALID_CURSOR`。
- **列表 ETag 用强验证器**（对响应字节做 md5）：契约 §10 标注列表用弱 ETag；精确字节哈希是更严格的验证器，语义成立，且与章节目录沿用同一实现。
- **`state` / `groupId` 过滤暂缓**：invalid 书源清单是独立缓存域、source group 是字符串字段而非实体，二者随写批次或 sources 域收尾时再定；本批 `q` 覆盖名称与 URL 的大小写不敏感子串。
- **前端 api 层适配过渡**：`getBookSourceList` 内部走 `/v1/sources` 游标分页拉全量并映射回现有 `BookSourceSummary`（组件与快照逻辑零改动）；组件全面消费生成类型时删除适配层。`getUserInfo` 改打 `/v1/me`，401 在 api 层收敛为 `userInfo: null`（与旧可选鉴权语义对齐）。me 的 stats/settings 随 me 域后续批次落地。

## 2026-11-09 审阅第五轮（sources 读链路收尾）

- **书源列表排序语义变更已有意记录**：旧 `/reader3/getBookSourceList` 是 DB `updated_at` 降序（最近编辑在前）；v1 固定为 `id` 升序（创建顺序，keyset 依赖不可变排序键）。前端 store 不再依赖服务端顺序，**管理页可见顺序随之改变**；如需恢复旧观感，客户端可按 `updatedAt`（源材料字段）本地重排——注意它不等于 DB `updated_at`。spec 的 `/v1/sources` 描述已写明排序。
- **`/v1/sources` 分页是「全量载入 + 内存切片」**：底层 `list_summary()` 每次把全部书源反序列化一遍，翻页会重复解析；几百个源可接受，书源量到万级时需下沉 SQL keyset（已标注在 `list_summary` 文档注释与 handler 注释，避免后人误以为已下沉）。
- **`parseSourceId` 是过渡期反向映射**（`src_<n>` → 数字 id，编辑器继续走 `/reader3/getBookSource`）：sources 写批次切 v1 时删除，并同步去掉这一层适配；不得沉淀成长期 API（与 `serverProgressTime` 同类风险）。
- **前端 v1 类型一律用生成的具名 schema**：`components['schemas']['SourceSummary']` 等别名，不手写接口（契约 §3.1）；后续 me 域铺开时同规则。

## 2026-11-09 P1 第一片：`GET /v1/books/{bookId}/chapters`

- **书籍 id 沿用 `book_<n>`，不引入契约 §5.1 示例里的 `bk_` 前缀**：`book_<n>` 已是既有线缆形态（`parse_book_id`/`format_book_id` 收口），再加一套前缀等于在每个 v1 域加翻译 shim；§5.1 的 `bk_` 是示意。sources 用 `src_` 是因为后端此前没有书源的线缆 id 形态，两者不矛盾。
- **章节列表数据来源三分支**：本地书走本地服务（目录由文件确定性生成，不写登记表）；网络书登记表优先，空则走 `/reader3` 同一份抓取服务填充登记表（首屏分页 + 后台补全的「先回首页」优化随阅读器切换再引入，本片同步全量抓取，入站 120s 超时兜底）。
- **URL 不出现在章节响应里**（红线 6）：身份是 `(bookId, index)`；标题覆盖层在出口叠加并显式给 `titleOverridden`。
- 暂缓项：Book 详情端点（`GET /v1/books/{id}` 需定 getBookInfo 语义与封面形态）与 content 端点（版本 ETag 语义设计）随阅读器切换批次一起做。

## 2026-11-10 P1 后端收官 + 阅读器切换方案

- **批量正文 `POST /v1/books/{id}/chapters:batchContent`**：并发 4、逐项成败（`error={code,detail}`）、顺序按请求归位；单章/批量共用 `map_to_v1`。
- **端点 spec 注册的流程教训**：批量端点首版提交漏了 `V1ApiDoc` 注册，漂移检查与清单断言都不报——生成物与锚文件一致缺失。以后每加端点必须同一提交内：`paths(...)` + `components(schemas(...))` + `REQUIRED_PATHS` + regen。

### 阅读器前端切换方案（下一批的接线表）

按依赖顺序（每条改动面单独提交，仍全绿）：

1. **api 层**：`bookshelf.ts` 增 v1 函数——`listChaptersV1(bookId)`（游标拉全量）、`getContentV1(bookId, index)`（带 If-None-Match）、`batchContentV1(bookId, indices)`、`patchProgressV1(bookId, progress)`、`getBookV1(bookId)`；旧 `/reader3` 函数保留到切换点删除。
2. **IndexedDB**：缓存条目改存 `etag`。单章 content 响应头带 ETag，可直接落；**批量接口没有响应头**，预加载条目的 etag 由 `items[].data.version.id`（`cv_<n>`）派生——与单章 GET 的 ETag 值完全一致。无版本内容（本地书上游原文，单章 GET 退回 `"body-<md5>"`）不参与条件请求，始终 200 全量（本地读取本身便宜），批量预加载的此类章节同理。读时带 `If-None-Match`，304 用本地正文，200 落新正文 + 新 etag。
3. **stores/readerContent**：章节列表走 v1（keyset 全量拉）；正文按 index 直读（不再拼 chapterUrl）；`refresh=1` → `refresh=true`；失败降级用本地缓存正文（同现状）。
4. **stores/bookshelf + 进度写入**：`saveBookProgress` → `PATCH /v1/me/shelf/{id}`；入架/移出/删除走 v1 三端点；`getBookshelf` 留 `/reader3`（书架列表域另切）。
5. **阅读器内评论/配图/编辑**继续走 `/reader3`（P3 批次）；目录/正文/进度切完即可删 `/reader3` 的 getChapterList/getBookContent/getBookContents/saveBookProgress。
6. **验收**：阅读器 e2e（reader.spec.ts）保持绿；新增一条 v1 正文往返用例（编辑版/上游版、`304` 缓存命中）。

## 2026-11-10 阅读器目录/正文/进度切 v1（前端适配层）

- **条件请求交给浏览器 HTTP 缓存，不在 IndexedDB 里存 etag**：单章 GET 响应带 `ETag + Cache-Control: private, no-cache`，XHR 对同一 URL 自动带 `If-None-Match` 并在 304 时透明复用缓存体；前端自管 etag 只是复制一层容易漂移的逻辑。`knownVersionId` 参数保留但服务端不再消费；批量接口的「unchanged」由客户端用 `version.id` 比对后等价化简（不重复写缓存）。
- **章节响应过渡期带只读 `url`**（红线 6 例外）：阅读器内的评论/配图/改名与 IndexedDB 缓存键仍用上游 URL，这些域迁 v1 后删除该字段。
- **目录读路径保留 TTL 语义**（完整 1h / 未完成 60s），但过期改为 stale-while-revalidate：先返回旧登记表并标 `stale`，后台单飞刷新；`refresh=true` 才同步重抓。分页续页只读裸登记行，避免翻页途中重抓打乱 index；失败保留旧登记行（不先删后抓）。
- **正文响应补齐阅读器消费的字段**：`edited` / `diffBaseVersionId` / `latestUserVersionId` / `diff.ops` / `lastAttemptAt` / `upstreamChangedAt` / `editedAt`；时间一律 RFC 3339，前端适配层转回旧形状的秒级数字。
- **进度写入统一走 v1**：`saveBookProgress` 与页面卸载的 keepalive 兜底都调用 `PATCH /v1/me/shelf/{id}`（keepalive 显式带 CSRF 头），`/reader3/saveBookProgress` 不再有前端调用。
- **浏览器整本缓存按 index 传参**：`cacheBookToBrowser` 原先只传 `chapterUrl`，补 `index` 后单章 GET 才能定位章节。

## 2026-11-10 审阅返工：正文 ETag 与目录 stale-while-revalidate

- **正文 ETag 改为整响应体 md5**：旧 `cv_<version_id>` 盖不住 `diff` / `diffBaseVersionId` / `upstreamChangedSinceBase` / `lastAttemptAt` / `stale`——上游推进时版本号不变，304 会让编辑视图永久停在旧 diff。body 哈希（强 ETag）一次算清；同时补回 `diffError`（服务端已有 `diff_error`，v1 之前漏发，阅读器 `chapterDiff.error` 消费）。
- **目录 TTL 过期改 stale-while-revalidate**：有登记行时普通读始终先返回旧目录，过期只标 `stale:true` 并在后台单飞刷新（复用 `try_begin_toc_fill`）；`refresh=true` 才同步重抓。修掉「TTL 过期 = 串行抓全部分页」与「上游挂 = 目录打不开」两个问题。`ChapterPage` 新增 `stale` / `complete`；前端对 stale 结果用 10s 短会话缓存，尽快换新。
- **首开仍同步抓全量**：登记表为空时没有旧目录可返回，只能同步抓（`/reader3` 的「首屏 + 后台补全」优化仍未引入，留待后续批次）。
- 批量正文的生产调用方是阅读器预加载；整本浏览器缓存（`cacheBookToBrowser`）仍逐章请求，批量接入标注待做。

## 2026-11-10 审阅返工：目录快照令牌与刷新竞态

- **`ChapterPage.tocVersion`（= `book_toc_state.fetched_at`）**：续页回传，与当前快照不一致时返回 **409 `STALE_CURSOR`**，客户端从第一页重拉一次。覆盖「后台刷新在翻页途中整表替换、index 重新编号」的接缝问题；不回传的旧客户端维持原行为。前端 `fetchAllChaptersV1` 捕获 `STALE_CURSOR` 自动重拉（不带 refresh）。
- **`complete` 仍参与客户端缓存策略**：`stale || !complete` 都按 10s 短会话缓存（未抓全的目录 60s 内会被补全覆盖，短 TTL 让读者尽快看到全量）。
- **TTL 判定只留一份**：`load_fresh_chapter_list_cache` 直接调用 `toc_state_is_fresh`，不再各写一份分支。
- **后台刷新写库前复核绑定**：`refresh_chapter_list_if_current` 在 replace 前比对登记行 `book_url` 是否仍是发起时的书；换源竞态下丢弃旧源结果。复核与 replace 之间仍有毫秒级窗口（已知取舍，写进方法注释）。
- **`refresh=true` 上游失败直接 502**：显式刷新不回退旧目录，如实报错；普通读才用 stale 兜底。spec 的 502 描述写明。

## 2026-11-10 书架写链路切 v1

- **`addBook` → `PUT /v1/me/shelf/{bookId}`**：201 新建时用调用方传入的展示字段就地追加（不整架重拉）；200 已在架时服务端不重置进度/元数据，本地条目原样保留（避免用搜索结果覆盖本地的书名/进度）。缺 `bookId` 直接报错——搜索/发现产出即登记，正常路径必有。
- **`removeBook` / `bulkDelete` → `DELETE /v1/me/shelf/{bookId}`**（批量逐本串行；v1 没有批量端点）。语义变化：不再顺带删书签、AI 记忆、图片与本地书文件，登记行与正文历史保留（重新加入接得上）。本地书文件因此不再随移出释放——这是「可恢复」语义的代价，彻底删除属于后续动作。
- **仍在 `/reader3` 的书架读域**：`getBookshelf`、`getShelfBook`（单条带缓存计数）、WebDAV 备份的 `saveBooks`/`restoreBooks`/`deleteBooks`。旧 api 的 `saveBook`/`deleteBook` 已无调用方，随本批删除。
