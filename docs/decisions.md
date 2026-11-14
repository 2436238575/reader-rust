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

## 2026-11-10 书架读链路切 v1

- **`GET /v1/me/shelf` + `GET /v1/me/shelf/{bookId}`**：返回 `ShelfItem`（注册信息快照 + progress/group/lastReadAt/cachedChapters/position），ETag/304，时间 RFC 3339，封面只给 `coverImageId`（取图仍走 `/reader3/image/<id>`）。游标按 `(position, bookId)`，与拖拽排序的位置列一致。
- **`sourceId` 用 `src_<n>`**：前端过渡期用 `parseSourceId` 反解成数字给旧组件（书源编辑/换源面板）。
- **列表不做 `/reader3` 的目录后台预取**：缓存章数来自正文池计数，预取只预热目录、不影响计数；去掉它等于「打开书架不再扇出抓几百个目录」。需要目录时按书读目录自然会抓。
- **不在架的单条读复用 `BOOK_NOT_IN_SHELF`（409，与 PATCH 同码）**；契约 §8.2 的 `?include=book,cache` 与 `?groupId/?q/?sort` 过滤未实现——前端仍本地过滤/排序，后续书架管理批次再下沉。
- **`getBookshelf`（无计数版本）仍留 `/reader3`**：WebDAV 备份快照需要 `tocUrl`/`infoHtml` 等 v1 书架条目不暴露的字段，备份域切换时一并迁移。

## 2026-11-11 书架批次返工

- **三条读取路径的排序键统一为 `(position, book_id)`**：`shelf list()` 原来只 `ORDER BY position`，与 `list_ids()` 及 v1 游标的 tie-break 不一致——position 一旦重复，翻页边界会静默跳书。一行 SQL 对齐。
- **空封面不派生图片 id**：`cover_url = Some("")` 会得出 `md5("")` 指向不存在的图；shelf 与 books 详情两处都加了空串过滤。
- **PUT `/v1/me/shelf/{bookId}` 接受可选展示字段**（name/author/coverUrl/intro/kind，`{}` 全缺省；**请求体本身必填**——无 body / 无 `Content-Type` 会 415/400，curl/脚本至少要给 `{}`）：带上即跳过回源详情，发现页直接入架、源已挂时不再等最坏 3×10s。本站取图地址传回时还原成上游地址落库（与 saveBook 的 restore_cover_urls 同语义）；空串不覆盖登记行真名。
- **`POST /v1/me/shelf:batchDelete`**：批量移出，逐本 `delete_book`（各自清缓存）不做整体事务，不存在/非法 id 忽略不计数，上限 1000。前端 `bulkDelete` 从 N 次串行 DELETE 改为一次调用——原来中途失败会「服务端删了一半、本地一个没动」。
- **前端 `addBook` 201 后补一次 `getShelfBook` 对齐**：封面统一成图片管道 id（搜索结果里可能是签名上游直出地址），只在新增时多一次请求。
- **缓存章数从每条 2 次 COUNT 减到 1 次**：`chapter_list_cache_exists` 的预探测冗余（count==0 已涵盖），且会把「有缓存正文但登记行被清」的书写成 0——直接 COUNT 反而更准。
- **待办（P4 前）**：本地书文件删除没有入口——移出书架不再删文件，磁盘只增不减。计划加 `DELETE /v1/local-books/{bookId}`（删文件 + 书架行 + 登记行 + 图片连带），与「移出书架可恢复」语义分开。

## 2026-11-11 缓存管理同步子集切 v1

- **交付**：`GET /v1/cache/stats`（typed `CacheStatsSnapshot` + ETag/304）、`DELETE /v1/cache?kind=`（按层/全部，同步执行）、`DELETE /v1/books/{bookId}/cache`（= `/reader3/deleteBookCache` 语义：重置目录抓取状态 + 删换源候选；登记行/章节登记表/正文历史/图片不动，幂等）。`cache_stats` 服务层从 `Value` 收敛成 typed 快照，`/reader3` 用同一形状序列化，两边线缆不变。
- **偏差（契约 §8.6）**：预下载（`POST /v1/books/{id}/cache`）与 policy 清理（`{policy: olderThan}`）按契约是 202 Task——任务子系统落地前不交付；`DELETE /v1/cache` 是同步按层/全部，不是 policy。r3 的 `username`/`all=true` 参数未带过来：v1 永远操作调用者自己的命名空间（单用户）。
- **留项（待拍板）**：书籍级清理拆「重置目录/候选」与「清除已下载正文」两个动作——拍板前 v1 只有现状语义，不引入删除正文历史的入口。

## 2026-11-11 书源写链路同步子集切 v1

- **交付**：`GET /v1/sources/{id}`（完整源材料透传，含数字 `bookSourceId` 字段——身份参数仍走 `src_<n>`；编辑器拿到的形状与 /reader3 一致，无需映射）、`POST /v1/sources`（整条 upsert：按自带 id 更新，否则按 URL 保号归并；201 新建 / 200 替换）、`POST /v1/sources:batchUpsert`（`{sources:[...]}`，URL 去重保号，单事务，上限 1000 条；该路由单独放宽请求体到 /reader3 同款 64MB——整站打包的导入文件远超 /v1 默认 1MB）、`DELETE /v1/sources/{id}`（幂等 204）、`POST /v1/sources:batchDelete`（忽略未知 id 不计数）、`DELETE /v1/sources`（body `{"confirm":"DELETE_ALL"}` 门槛，同步清空）、`POST /v1/sources:deleteInvalid`（分组含「失效」的整行删除）。
- **请求体是源材料**：走 `book_source_from_value` 的宽松迁移（legado 旧字段名 `enable`/`serialNumber` 等兼容），不做 strict——导入材料带附加字段是常态，这与 replace-rules 的 strict 是两个刻意的不同（D16 的例外，已记录）。
- **偏差**：契约的 `POST /v1/sources` 带 `Idempotency-Key` + `Location`、`:import`/`:test`/`:login` 202 Task、`PUT /v1/sources/{id}`——任务子系统（P2）落地前不交付；前端编辑保存走 POST upsert（r3 语义）。`PATCH /v1/sources/{id}`（轻量启停）随启停域迁移。`parseSourceId`/`buildSourceId` 仍留在 api 适配层：列表/详情的旧消费组件还吃数字 id，全面换 v1 类型时删除。
- **repo 语义补强**：`book_sources` 的 `delete_by_id`/`delete_all` 返回删除行数（v1 批量计数用），r3 调用点不受影响。

## 2026-11-11 书签/分组域切 v1

- **书签（marks）**：`GET /v1/me/marks?bookId=`、`POST /v1/me/marks`（同身份覆盖）、`POST /v1/me/marks:batch`（WebDAV 恢复，单事务；指向未登记书的条目跳过并计 `skipped`）、`POST /v1/me/marks:batchDelete`（按身份五元组，幂等计删除数）。**偏差**：审计里的 `DELETE /v1/me/marks/{id}` 没有可实现的自然 id——书签身份是五元组（无 id 列），单删并入 `:batchDelete` 单条复用。`POST /v1/me/marks` 对未登记书返回 404 BOOK_NOT_FOUND（r3 同义）。
- **分组**：`GET /v1/me/groups`、`POST /v1/me/groups`（`groupId=0` 新建分配空位幂，非 0 合法幂位更新）、`PATCH /v1/me/groups:order`（只改顺序）、`DELETE /v1/me/groups/{groupId}`（组行 + 书籍位掩码清位，幂等）。分组语义与 `/reader3` 完全一致（save_group 的幂位分配与 reorder 的只改序都直接复用 service）。
- **书架 PATCH 补 `group` 字段**：单本书的分组归属从 r3 `saveBookGroupId` 迁到 `PATCH /v1/me/shelf/{bookId}` 的 merge-patch（`group: 0` = 移出全部分组；非 0 必须是已存在的合法幂位，否则 422 带 `/group` pointer）。`group` 与 `progress` 相互独立，只传其一；仅改分组不刷新最近阅读时间。审计里的 `PUT /v1/me/groups/{id}/books`（整体替换成员）与 `:batchAssign` 未交付（前端无此调用）。
- **userdata（getUserdataBatch/saveUserdataBatch）不迁**：契约把 userdata 文档替换为 `/v1/me/stats`（服务端从进度派生，D9 read_events）——那是产品语义重设计，不是端点翻译，单独批次拍板后再做。
- `bookmarks` repo 的 `delete` 返回是否删行（v1 批量计数用），r3 调用点不受影响。

## 2026-11-12 审阅返工：批量体限与书源 upsert 状态码

- **marks:batch 的体限与声明上限对齐**：路由级 `DefaultBodyLimit` 提到 8MB（与 5000 条 × 真实划线约 1.5KB 匹配，挂在 MethodRouter 内层）；客户端 `saveBookmarks` 按 500 分片串行（与 batchRemoveFromShelf 同一模式）——否则 WebDAV 恢复一次全发会撞 /v1 默认 1MB，且 413 提示会把人引去查 nginx。新增 >1MB（2000 条带选文 ≈2.4MB）与 5001 条超限两条测试，把「上限可达」钉住。
- **sources:batchUpsert 的 64MB 补测试**：2MB jsLib 的批量导入用例锁住「内层 layer 覆盖外层 1MB」的层序——这是全仓库唯一的该用法，没有测试兜底就靠运气。
- **`POST /v1/sources` 的 201/200 判定修正**：自带 `bookSourceId` 命中（编辑改 URL 走 update 分支）也算替换，只有全新建才 201——只看 URL 会把改地址的编辑误报「已新建」。
- **分组保留位掩码（决定，非延后待办）**：计划文档里写的「groups（关联表，废除位掩码）」**不执行**——位掩码是现状存储（`book_groups.group_id` 位域 + `shelf_books.group_bits`）与阅读3.0 的兼容口径，`group: 3` 这类直接赋值已被幂位校验挡住（saveBookGroupId 与书架 PATCH group 同一道闸）。关联表只在「单书需要归属任意多个组且各自独立管理」成为需求时再立项；在那之前 v1 与 /reader3 的分组语义保持逐字一致。

## 2026-11-12 评论/配图读链路切 v1（P3 前半）

- **交付**：`GET /v1/books/{bookId}/chapters/{index}/comments`（章评与段评合并在同一路径：`?para=<段号>` 是段评，`?authorOnly=true` 忽略分页扫描作者评论）、`GET .../comments/index`（段评概览）、`GET .../images`（章节配图）。响应就是服务层的 typed 形状（`ReviewResponse<ReviewPage>` / `ReviewResponse<ParaReviewIndex>` / `ChapterImages`），评论解析、7 天缓存、空页不缓存、`serverSort`/`authorMarks` 全部复用 /reader3 同一套服务。
- **红线 6 落地**：客户端不再携带 chapterUrl——v1 以 `(bookId, index)` 经章节登记表/本地目录解析出内部地址（复用 content 的 `resolve_chapter_url`）；前端 `ReviewParams` 与 `CommentPanel` 的 prop 同步从 `chapterUrl` 换成 `index`。
- **配图收口 typed 化**：`ImageService::pick_image_route`（`rewrite_image_lists` 的单格语义）——同一张图的多个格式变体挑一张能渲染的，替换成单个 `/reader3/image/<id>`；挑不出/登记失败保留原数组（行为与 /reader3 一致）。评论项与回复都收口；章节配图逐张登记。
- **偏差**：契约 §8.5 的 `anchor`（blockMap 对齐的评论锚点）随 mark-anchoring 批次再做；响应形状沿用现状（前端零映射），`Page<Comment>` 的理想分页形状（cursor）等编辑域一起收敛。

## 2026-11-12 子 agent 审查返工（P3 评论/配图/编辑域）

- **P0 修复**：`rename_chapter` 在 `title_context` 找不到章节时对空 `chapter_url` upsert 覆盖层——加守卫拒绝（400 ChapterNotFound）。触发面窄（换源竞态）但落库脏数据 + 谎报成功。
- **P1 修复**：
  - `get_chapter_edit` / `get_chapter_version` / `get_chapter_version_diff` 三个端点从 `serde_json::Value` 改为 typed schema（`ChapterEditViewPayload` / `ChapterVersionDetailPayload` / `ChapterDiffPayload`），红线 5 兑现。
  - `PUT content` 与 `POST content:import` 挂路由级体限（8MB / 100MB）：富 HTML 章 / 单章导入大文本不再 413。
  - `title_context` 对 `load_chapter_list_cache` 失败改回吞错误按空原标题继续（与 /reader3 口径一致）。
  - 删除前端死代码 `listChapterEdits`（无调用方）；`parseVersionId` 加 `Number.isSafeInteger && > 0` 校验；清理测试 debug println。
- **P1-3 不改（有意保留）**：编辑域端点在网络书登记表为空时走 `resolve_chapter_url` 触发目录抓取——与正文 GET `content` 同一行为，正常阅读路径会先经目录端点填表。是否改为「登记表 miss 即 404」随编辑域后续迭代决定。
- **P1-4/P1-5 不改（已有覆盖）**：import-diff 端点的核心逻辑由 /reader3 的 `tests/chapter_edit_flow.rs` 间接覆盖（共享 `chapter_import_diff_payload`）；v1 评论测试的 authorOnly / refresh / variant 分支由 /reader3 `tests/review_flow.rs` 覆盖（共享同一服务调用）。v1 端点是薄包装（index→url + typed 映射），共享核心已有回归保护。
- **分组保留位掩码（决定，非待办）**：计划写的「groups（关联表，废除位掩码）」不执行；关联表触发条件是「单书归属任意多组且独立管理」。

## 2026-11-12 章节编辑域切 v1（P3 收官）

- **交付**：16 个端点——`PUT/DELETE .../content`（编辑保存/取消）、`GET .../content/edit`（编辑器预填）、`GET .../versions`（时间线）、`GET .../versions/{cv}`、`GET .../versions/{cv}/diff?against=`、`POST .../content:select`、`POST .../content:import`、`PATCH /v1/books/{id}/chapters/{index}`（改名）、`GET/DELETE .../title`、`GET /v1/books/{id}/chapter-marks`、`POST /v1/books/{id}/content:reset`，以及导入链 `chapters:preview-import` / `chapters:import` / `chapters:upload-import-chunk` / `chapters/import-diff`。服务调用全部复用 `chapter_content_service` / `chapter_diff_service` / 导入核心（r3 与 v1 共用的 typed payload）。
- **写路径不做 `If-Match` / 412（有意）**：版本是追加式的、历史严格保留——两个客户端并发保存只会产生两个版本，晚到的成为当前版本，先前版本仍可在时间线里重新选中，不是不可逆覆盖。`baseVersionId` 只作 diff 基准，不承担并发令牌职责。契约 §8.4 的 `If-Match`/412 条目作废（Draft-2 §16.2 已记录；生成物 `openapi.yaml` 从未包含该参数）。
- **`chapterUrl` 从线缆消失**：编辑域全部按 `(bookId, index)`，index 经章节登记表/本地目录解析出内部地址（红线 6），与评论/配图同一口径。`chapter-marks` 的键暂用内部 URL（与 `ChapterItem.url` 同生命周期，过渡期例外；URL 字段删除时一并改 index 键）。
- **导入子路由在 120s 超时与 1MB 体限之外 merge**：整本 TXT 预览/导入/分片上传自带 900s 上传超时 + 64MB 体限 + 鉴权 + CSRF；与 `/reader3` big_body_routes 同一做法，避免大文件被常规超时误杀。
- **`GET .../content/edit` 的 404 语义**：没有用户版本时返回 404 `CHAPTER_EDIT_NOT_FOUND`（新 ErrorCode），不是 200 + null——客户端据此区分「没有编辑」与「网络失败」。
- **`reset` 只清指针**：整书回上游只摘版本选择与用户版本指针，历史版本严格保留；**没有 confirm 门槛**（与 `DELETE /v1/sources` 的 `DELETE_ALL` 不同：reset 可逆、风险可控）。
- **`content:select` 校验版本归属**：跨章 select 返回 400（服务层统一闸，`/reader3` 一并受益）；前端版本面板已加入 `isBlockingOverlayOpen`，modal 打开时翻页键不再生效——两层防线缺一不可。

## 2026-11-13 非 P2 同步端点：探索发现与本地书上传

- **`GET /v1/sources/{src_<n>}/explore?url=&page=`**：按书源 exploreUrl 分类地址拉书单；响应 `Vec<SearchBook>`（typed），封面统一收口为 `/reader3/image/<id>`；搜索/探索产出即登记（响应带 `bookId`）。
- **`POST /v1/local-books`**：本地书上传（multipart `file`），TXT/EPUB 按扩展名分派；读体上限按分派结果选对（EPUB 80MB / TXT 50MB），修复了「50–80MB EPUB 被 TXT 50MB 上限误杀」的回归。路由在 120s 超时与 1MB 体限之外 merge（子路由自带 900s 上传超时 + 100MB 体限 + 鉴权 + CSRF），与导入链同一层序。
- **`?kind=` 的覆盖语义**：`kind=epub` 或 `kind=txt` 显式覆盖扩展名分派（原来 OR 逻辑会把 `kind=txt` + `.epub` 文件名判成 EPUB）；`kind=garbage` 显式 400 不再静默回落扩展名。底层服务的扩展名校验不越权——`kind=txt` + `.epub` 文件名仍被 TXT 扩展名校验拒绝。
- **explore `url` 不限定在书源声明的 exploreUrl 内**（与 /reader3 一致，非回归）：认证用户可传任意地址让服务端用书源规则抓取；Cookie 按域隔离不会外泄。如需封路可加「url ∈ exploreUrl 候选」校验，暂不做。
- **e2e 种子数据切到 v1**：`reader.spec.ts` 的 `uploadTxtBook` fetch 路径改为 `/v1/local-books`，整套 e2e 顺带验证新端点。

## 2026-11-13 审阅返工：详情覆盖层、本地书详情与门禁收紧

- **v1 详情是覆盖层，不是完整 Book**：`getBookInfo` 映射成 `BookInfoOverlay`（`BookDetail` 确实给出的字段），**不补 `origin: ''`**——旧实现把空串塞进合并结果，`{ ...store.book, ...latest }` 会覆盖书架条目里的书源 URL/名称，详情弹窗的「书源」行整块消失。`useBookInfoModal` 合并前再过滤一遍 `undefined` 键（双保险）。
- **替换规则名：空白放行**：上一版自动命名（`书源替换 3`）已经落库，路径经 `encodeURIComponent` 编码后 axum 能解回空格；校验收窄为「拒 `/ % ? #` + 控制字符」，历史规则改文案/删除不再 422。制表/换行属 Unicode Cc，仍拒绝。
- **`GET /v1/books/{bookId}` 补本地书分支**：TXT/EPUB 走本地索引（不触碰上游），`totalChapters`/`lastChapter`/`kind` 与 legacy `/reader3/getBookInfo` 对齐，`stale` 恒 false。
- **本地 EPUB 封面给 `coverUrl`**：封面是站内资源地址（`/reader3/localEpubAsset?...`），图片管道只收 http(s)（`ImageService::register` 的守卫），`coverImageId` 表达不了。`BookDetail` 与 `ShelfItem` 新增 `coverUrl`（与 `coverImageId` 二选一，仅站内资源；http(s) 上游地址仍只给 id，不破坏红线 6）。`split_cover` 是唯一拆分点，书架与详情共用。
- **漂移门禁改精确匹配**：`REQUIRED_PATHS` 写完整路径、断言按 YAML `paths` 的两空格缩进键整行比较。`contains("/…/content:")` 会把 `…/content:select:` 也命中——一个端点消失能被另一个顶包（`content` / `content:select` / `content:import` 三者前缀重叠）。清单同步补全到全部已交付路径。
- **替换规则管理弹窗标题**：v1 按 `name` 做身份、线缆无 `id`（适配层统一补 0），「编辑/新增」标题改用打开弹窗时的 `isNewRule` 判断，不再看恒 0 的 `id`。

## 2026-11-13 任务子系统首批：kind=search 全链路（P2）

- **范围（D13）**：`kind=search` 全链路（注册 `POST /v1/searches` → 状态/事件/取消/结果分页）打通后，继续接入 `kind=prefetch-cache`、`kind=source-test`、`kind=source-import`；四类工作器复用同一注册表/事件/结果机制。`source-test`/`source-import` 默认不写库（测试是读操作，`markInvalid=true` 才写失效分组）。
- **prefetch-cache（`POST /v1/books/{bookId}/cache`）**：body `{from, count}`（from 起始章下标缺省 0，count 1..=1000 缺省 50）→ 202 Task；本地书直接 422（内容本来就在本地）。worker 先查正文池跳过已缓存章节，再以**并发 8** 调 `cache_chapter`（与旧 SSE 路径前端传的 8 一致；全局出站预算 32 兜总量）；每章一个取消检查点（派发前后与 `select!`），取消落地即 abort 在飞抓取。结果按章计数 `{requested, cached, skipped, failed}`；全部失败 → 任务 failed，部分失败保留摘要。`GET /v1/tasks/{id}/result` 按 kind 返回 `search` 分页或 prefetch 摘要（OpenAPI oneOf `TaskResultResponse`）。**前端「全本」超过单任务 1000 章上限时串行续排下一批**，进度与汇总按整本书累计，不再静默截断。
- **source-test（`POST /v1/sources:test`、`POST /v1/sources/{sourceId}:test`）**：`{ids?, keyword?}` / `?markInvalid=false`（默认 false）→ 202 Task；逐源跑搜索+发现，结果 `SourceTestResult`（valid/invalid/markedInvalid + 每源 searchOk/exploreOk/错误）。`markInvalid=true` 才写「失效」分组。matchit 不允许 `{param}:suffix` 同段，`{sourceId}:test` 由 `{sourceId}` 的 POST 承接并在 handler 里剥后缀——外部路径与契约一致。
- **source-import（`POST /v1/sources:import`）**：JSON `{sources: []}`（兼容数组/`bookSources`/旧字段名）或 multipart JSON 文件（单文件 32MB、请求体 64MB）→ 202 Task；逐条 `book_source_from_value` 宽松迁移后保存，结果 `SourceImportResult {total, imported, failed, errors[]}`；全部失败 → 任务 failed。支持 `Idempotency-Key`。
- **注册表是单进程内存表**：进程重启丢任务（客户端按 404 重发）。任务 id 序号以随机 64 位基数起算，重启后旧 id 不会指向新任务。任务与结果保留 24h，但容量压力下会**提前驱逐已结束任务**（GET 变 404）：注册表最多 64 个任务、每任务结果 10MB、全局结果预算 128MB、每任务事件环 256 条且 256KB。`item` 事件按 32KB 分块（单本超大结果不发 SSE，客户端走 result 分页），保证每条事件都能留在环里被 `Last-Event-ID` 重放；合并补齐字段同样受结果预算约束（不许后续书源补超大字段顶穿上限）。
- **并发上限**：全局 8、每用户 4；超限 429 `TASK_LIMIT` + `Retry-After`。取消中（`cancelling`）仍占名额，工作器落地 `cancelled` 后释放。**后台长任务（预下载/章节导入/书源导入）最多占「上限 - 1」**（每用户 3、全局 7），给搜索/发现保留即时名额；后台任务被拒时 `Retry-After: 60`，交互任务因后台任务占满而 429 时也用 60——5 秒重试对几十分钟的预下载没有意义。**source-test 有意不归后台**：它是用户盯着看的前台操作，即使测 1000 个源也占交互名额，避免测试洪峰把预下载等长任务饿死。
- **全局出站预算**：任务在后台 tokio task 里抓上游，不经过 `/v1` 的入站并发层。新增 `TASK_OUTBOUND_CONCURRENCY`（默认 32，`0` 按默认）配置一个跨任务共享的信号量，搜索每个书源、预下载每章在发请求前 acquire；任务内并发仍是搜索 8/预下载 4，但所有任务叠加的上游连接数不超过该预算。
- **取消是协作式的**：`DELETE` 置位并唤醒工作器（状态先变 `cancelling`），工作器在调度循环的 `select!` 里立即察觉，abort 在飞任务后落 `cancelled`；派发循环每轮 spawn 前与子任务真正发请求前各查一次标志位，取消后不再发起新上游请求。封面收口（逐个 await）与取消通知竞争，取消落地即 abort 其余书源；子任务用 `JoinSet` 持有（析构自动 abort，panic 不会留下孤儿请求）。`finish` 尊重已发出的取消（`cancelling` + 非 `cancelled` 终态 → 强制 `cancelled`）；worker 有 Drop guard 落 Failed，DELETE 另挂 5 秒 watchdog 兜底（重复 DELETE 不重复挂；超时 abort 工作器，JoinSet 连带终止在飞请求），避免卡死任务永久占名额。未知任务 404 `TASK_NOT_FOUND`（已结束任务仍 204）。
- **事件 id 与续传**：每个任务内事件 id 单调递增，重连带 `Last-Event-ID` 只重放之后的条目；`done` 是最后一条事件（终态任务的 replay 必含它），**不合成重复 done**；`Last-Event-ID` 早于缓冲起点时只重放保留后缀（`TaskSubscription::gap` 记日志），客户端用任务状态/结果分页对账——事件流不是唯一真相。订阅者落后于 broadcast 时直接关流，让 EventSource 带 `Last-Event-ID` 重连补齐；每任务最多 4 个并发订阅者。`gap` 只记日志不额外发事件（立场：只吃 SSE 的客户端靠 `cursor` 跳变自检；补一个专用 gap 事件会污染固定四种事件名的契约）。
- **结果游标是 offset（立即接受）**：任务运行中新结果到达会让分页窗口平移，翻页可能重复/跳过；客户端通常在任务结束后才分页，改成 keyset 需要给每条结果编号、收益不足。跨任务/无法解码的游标仍返回 422。
- **结果 total 语义**：`total` = 去重后**实际保留**的条数（截断后只算保留部分），`truncated` 单独标记「还有条目被丢弃」；结果顺序是到达顺序，不是最终排序。结果游标绑定任务序号，跨任务使用返回 422 `INVALID_CURSOR`。
- **`dedupeKey` 归一化规则**：全角→半角、去空白、ASCII 小写、繁→简（`zhhz`，Apache-2.0 纯 Rust）、剥**已知**书名后缀（完结/全本/全集/无弹窗/txt下载 等，含 `(完结)` 这类装饰括号）。带语义/版本含义的括号与词（`三体（纪念版）`、`精校版`、`未删节`）不剥——宁可不合并，也不误并。`dedupeKey` 是 `["书名","作者"]` 的 JSON 编码（避免 `|` 碰撞）；缺书名时退化为按 `bookUrl` 唯一；剥完为空时回退到未剥文本。
- **搜索源的选择**：`sourceIds` 与 `groupId` 二选一（`group` 作为兼容别名）；缺省搜全部**启用**书源。显式指定停用源 → 409 `SOURCE_DISABLED`；分组按 `split_source_groups` 的权威口径拆分后精确相等（`"玄幻"` 不命中 `"玄幻小说"`）。参数上限：`q` ≤200 字符、`sourceIds` ≤64 个、`limitPerSource` 1..=100。
- **偏差（Draft-2 §16.2 候选）**：契约 §8.6 的 `groupId` 值仍是**字符串分组名**（书源分组不是实体 id）；`dedupe` 目前只接受 `name-author`。搜索结果仍用 legacy `SearchBook` 形状（数字 `bookSourceId`、字符串 `lastChapter`、`bookUrl`/`origin` 直出）——与契约 §9.2 的 `BookSummary` 差异待搜索域产品收敛时统一；`bookUrl`/`origin` 与 explore 的 `ChapterItem.url` 同属红线 6 过渡例外。
- **状态码口径**：值级校验（缺 q、limitPerSource 越界）→ 422 `VALIDATION_FAILED` + pointer；未知枚举值（`dedupe`/`state`）与 query `limit` 越界 → 400 `MALFORMED_REQUEST`（契约 §5.5）。`V1Json` 的未知字段仍 422 `UNKNOWN_FIELD`，其余字段缺失/类型不符改为 400（此前一律 422 UNKNOWN_FIELD，会把「少传字段」误报成「未知字段」）。任务相关 query 参数统一走新的 `V1Query` 提取器，避免 axum 的纯文本 400 泄漏。
- **SSE 票据（D11/契约 §6.4）**：`POST /v1/sse-tickets {path}` → 201 `{ticket, expiresAt}` + `Cache-Control: no-store`；60 秒、一次性、绑定单个资源路径，只对 `/v1/tasks/{id}/events` 生效。鉴权顺序：Authorization → Cookie → `?ticket=`（仅事件流 GET），票据校验通过即消费；路径不匹配不烧票；消费后复核 `users.token_version`（改密码/登出后旧票立即失效）。每用户最多 8 张未消费票据、全局 1024 张，超限 429 + `Retry-After: 60`。
- **前端切任务模型**：`api/tasks.ts` 统一封装 `POST /v1/searches`、`POST /v1/books/{id}/cache`、`POST /v1/sources:test`、任务状态/结果/取消与 SSE 消费；`runTask` 用原生 `EventSource`（自动带 `Last-Event-ID`）+ 2 秒轮询 `GET /v1/tasks/{id}` 兜底，`close()` 会 DELETE 取消任务。**任务过期/会话失效（轮询 404/401、EventSource CLOSED）会收敛为 failed 并提示重新搜索**，不会永远停在「正在重连」。搜索页改为建任务 + item 事件增量渲染 + 结束时用 result 页按 bookId 去重补齐；缓存面板（阅读器内与缓存库弹窗）改为预下载任务，完成时读 `PrefetchSummary`（cached/skipped/failed）出提示，**「全本」超过 1000 章会串行续排下一批**（整本累计进度与汇总，不静默截断；服务端目录比前端 store 短时按本批 `requested < count` 停止续排，避免下一批越过目录末尾 422）；书源管理页的「测试」改为 `sources:test` 任务（按 1000 分片串行），不再请求 `/reader3/testBookSources`，离开页面时取消当前测试任务。换源候选 SSE 仍走 `/reader3`（随换源域迁移）。
- **`Idempotency-Key`（契约 §5.6）**：`POST /v1/searches` 接受 `Idempotency-Key`（≤200 字符），查找与注册在同一把锁内完成，弱网并发重试返回原任务（不同 body 也返回原任务）；映射随任务 TTL/容量驱逐清理。
- **spec 精度**：`q`/`path` 用 `Option` + handler 校验以返回 422 + pointer，同时用 `#[schema(required = true)]` 把必填性写回 spec；任务资源的 nullable 字段（`startedAt`/`finishedAt`/`resultHref`/`error`/`nextCursor` 等）标 required（值可为 null），`limit`/`limitPerSource` 写入 min/max，`sourceIds`/`q` 写入 maxItems/maxLength。`tests/openapi_drift.rs` 对这些回归点有定点断言。
- **待办（P2 收尾后）**：书源登录 `:login`/代理页与 `PATCH /v1/sources/{id}`（随启停域）、换源候选 SSE（`getAvailableBookSourceSSE`）、任务级 `traceId` 与失败源清单（目前部分源失败只记日志）、其他 `/v1` 查询参数迁移到 `V1Query`。
- **新依赖**：`zhhz = 0.8`（繁简折叠）。选它而不是 `zhconv`：Apache-2.0（zhconv 是 GPL-2.0-or-later），纯 Rust 数据内嵌、无 C 依赖。

## 2026-11-14 WebDAV 域纳入 /v1（协议挂载 + 文件浏览 + 备份恢复）

- **JSON 资源名用 `/v1/me/files`，不复用协议名**：`webdav` 是传输协议，不是资源身份；`/v1/me/...` 下操作的是当前用户的服务器文件。`/v1/me/webdav` 读起来像「一个叫 webdav 的资源」，与 `/v1/me/shelf`、`/v1/me/marks` 的风格也不一致。
- **备份端点放 `/v1/me/backups`，不塞进 `/v1/me/files/backups`**：文件下载/删除的 `{*path}` 是通配路径，静态段 `backups` 会与真实目录 `backups` 抢同一路径——静态路由优先后，该目录本身再也无法通过 API 下载/删除。备份是独立的资源集合，单列两个端点是更干净的切分。
- **协议入口新增 `/v1/webdav`，`/reader3/webdav` 保留**：第三方 DAV 客户端需要协议根；旧客户端已配置 `/reader3/webdav`，删除是破坏性变更。三个入口（`/v1/webdav`、`/reader3/webdav` 两个前缀，加各自的裸根/尾斜杠/通配路由）共用 `storage/webdav/<ns>` 与同一套 Basic 认证/限速/路径加固，只有 dav-server 的 strip 前缀不同。协议路由仍在压缩层之后 merge（dav-server 的流式 body 重复轮询会 panic），也因此豁免入站 120s 超时与 JWT/CSRF。
- **备份改为服务端快照**：旧前端逐接口拉书架/分组/书源/书签/规则再拼 JSON，恢复同样由前端逐接口清空重建；现在 `POST /v1/me/backups` 直接由 `WebdavBackupService` 生成文档并原子写入 `/backups/reader-backup-<UTC>-<随机后缀>.json`。备份字段与旧格式逐字兼容（`bookshelf.books/groups`、`bookSources`、`bookmarks`、`replaceRules`、`localState`），旧备份可直接恢复；`app` 标记新文件为 `reader-rust`。**必填域严格反序列化**：缺 `bookshelf`/四个数组任一即 422，绝不按空数组继续（否则 `{"version":1}` 就能清空全部数据）。
- **`localState` 前端采集、服务端透传**：创建备份时前端把白名单键放进请求体，服务端原样存回文档；恢复时原样返回。`null`（字段缺失的旧备份）与显式空对象语义不同：前者前端不动本地键，后者按整份覆盖。服务端不解释白名单——那是浏览器存储布局。
- **恢复语义**：书源清空重导（空表上 `bookSourceId` 原位保留）、分组整表覆盖（不在备份里的组删除并清书籍位掩码）、书架 `save_books(restore=true)`（只清当前用户书架，登记行与正文历史保留）、书签/替换规则 `replace_all` 清空重建。非法分组 id（旧版 `max_id+1` 遗留的 3）按旧前端同口径丢弃。**五个域共用同一个 `BEGIN IMMEDIATE` 事务**（repo 增加 `_on(&mut tx)` 变体，`save_books_on`/`save_groups_on`/`replace_all_on` 串联）：任何一步失败整体回滚，不再有「书源已换、书架已清」的半恢复状态；写库前集中预校验版本=1、书籍 URL/数量/ID、书签 kind/chapterPos，校验失败不开启事务。同一进程只允许一个恢复（`try_lock` 失败 409），避免并发恢复互相覆盖。`:inspect` 在恢复前返回条目数，前端确认框展示并提示空书源会清空现有书源。
- **bookId 以恢复后的实际身份为准**：`save_books(restore=true)` 会用备份自带的 bookId 原位登记，但同 URL 的既有登记行优先复用原 id、备份 id 被别的 URL 占用时重新分配。恢复时用「备份 id → 实际 id」映射重写书签的 `bookId` 与替换规则的 `book:` / `source:` scope；**书架外的书签**在新备份里带 `bookUrl`，按 URL 找回登记行（旧备份没有该字段则计入 `skippedBookmarks`），避免按旧 id 挂错或静默丢数据。书籍的 `bookSourceId` 也按恢复后的 source id 重写，备份里没有的源一律解绑。`save_books` 的同一套预校验（先规范化 URL 再查数量/ID）抽成 `normalize_books_for_restore` + `validate_restore_books`，恢复在任何删除之前调用。
- **文件接口细节**：列表 `modifiedAt` 用 RFC 3339 UTC（与 v1 时间口径一致），响应带强 ETag/304；下载带元数据 ETag、显式 Content-Type 与 `sandbox` CSP（`/reader3/getWebdavFile` 同一条中间件）；上传沿用 100MB/20 字段/流式落盘/取消清理，目标位置是符号链接时显式 400（与下载/删除的 canonicalize 守卫同口径）；批删先预校验全部路径再执行，单次上限 1000（前端 500 分片）。
- **前端接线**：`api/webdav.ts` 全部切到 `/v1`；`WebdavView` 的「备份」直接调服务端快照，「恢复」调服务端恢复后只应用 `localState`。旧的 `utils/webdavBackup.ts`（前端拼装备份/恢复编排）与其单测删除，白名单逻辑迁到 `utils/webdavLocalState.ts` 并有单测。
- **兼容面**：`/reader3` 的 JSON 文件接口与协议入口继续可用；前端已无调用方，保留给第三方脚本与已配置的 DAV 客户端。

## 2026-11-14 书架排序 / 缓存内检索 / 换源候选 SSE / userdata 批量写

- **书架排序用 `PATCH /v1/me/shelf:order`**：body `{bookIds}` 必须是当前书架完整集合；服务端只写 `position` 一列（`save_book_order` 既有语义），空/重复/非法 id/集合不一致 → **422 + `/bookIds` pointer**「书架已变化，请刷新后重试」。前端拖拽/置顶从 legacy `saveBooks`（整架 DELETE+重插，会覆盖未提交列）切到该端点；失败会回滚并拉一次最新书架。`saveBooks` 只保留给旧客户端与恢复兼容。
- **缓存内全文检索用 `GET /v1/books/{bookId}/cache-search?q=&limit=`**：只扫已缓存正文、不触发上游；`q` 1..=100 字符、`limit` 1..=200（缺省 50），值级非法 → 422 + `/q`/`/limit` pointer。响应去掉 `chapterUrl`（红线 6），前端只消费 `chapterIndex`；本地书不在检索范围。耗时/内存预算仍由服务层 5000 章/50MB/10s 与并发 2 控制。
- **换源候选 SSE 用 `GET /v1/books/{bookId}/source-candidates/events`**：复用 legacy `build_available_source_sse`（只换入口/鉴权/typed query），事件帧仍是 `message {lastIndex, hasMore, data:[SearchBook]}` + `end`。`refresh` 改 bool、`lastIndex >= -1`、`concurrentCount 1..=20`，越界 422；书籍不存在收敛为 404 `BOOK_NOT_FOUND`。**候选逐源搜索也 acquire 任务的全局出站信号量**（`TASK_OUTBOUND_CONCURRENCY`），与搜索/预下载共用同一份预算，不再是 32+20 的例外。前端 `authedEventSource` 增加可选 baseUrl，EventSource 从 `/reader3` 切到 `/v1`；`/reader3/getAvailableBookSourceSSE` 保留。
- **userdata 批量写用 `PUT /v1/me/userdata:batch`**：body `{docs:[{name,value}]}`，最多 20 个文档、单文档序列化 ≤4MB、请求体 8MB；文档名白名单与 legacy 同口径，空批次/超限/非法名/缺 value → 422 + pointer，单个文档超 4MB → 413；同名最后写入为准，全部校验后单事务落库。前端 `api/userdata.saveUserdataBatch` 与 `userdataQueue` 的 pagehide keepalive（PUT + CSRF 头）切到 v1。**批量读 `getUserdataBatch` 仍留 `/reader3`**：契约的 `/v1/me/stats` 派生设计未拍板，本次只迁写入；读接口切换随 userdata 产品语义重设计一起做。

## 2026-11-14 书源域收尾：文件解析/拉取、登录、代理票据、导出、换源落地

- **文件解析 `POST /v1/sources:parse`**：multipart `file`（`.json/.txt`，≤32MB），只解析不落库，返回 `[BookSource]` 供前端预览后再 `:batchUpsert`。路由在 v1 常规 120s/1MB 之外 merge，自带 900s 上传超时与 32MB 体限；非法 JSON/源材料 422，超限 413。
- **远端拉取 `POST /v1/sources:fetch` 用严格公网 SSRF 策略**：`url_guard` 新增 `public_only_client_builder` + `ensure_public_outbound_url_str_allowed`，**忽略部署级 `ALLOW_PRIVATE_NETWORK`/白名单**——客户端给任意 URL，服务端只能访问公网。只允许 http/https；DNS 解析（含重定向后的解析）拒绝环回/私网/链路本地/云元数据/保留地址；重定向逐跳拒绝内网字面地址；15 秒超时、响应 ≤32MB。这是有意的口径收窄：普通书源抓取仍可按配置访问局域网，只有 v1 这个「拉一份书源文件」的入口永远只走公网；legacy `/reader3/readRemoteSourceFile` 仍按 `ALLOW_PRIVATE_NETWORK`/白名单放行，保留给局域网自托管场景。错误 detail 明确写「远程导入只允许公网地址（不受私网白名单影响）」，避免自建用户困惑。
- **登录 `POST /v1/sources/{sourceId}/login`**：跑 loginUrl + loginCheckJs，成功 Cookie 落在该用户的独立 reqwest Cookie jar；返回 `{ok, message, cookieCount, previewUrl?}`。`cookieCount` 只统计最终响应 `Set-Cookie` 条数（跳转中间响应无法用 reqwest 公开 API 枚举）；失败（HTTP 非 2xx 或 loginCheckJs 显式 false/空）→ 422 `SOURCE_LOGIN_FAILED`，message 只含状态码/校验结果，**不回显 Cookie 值或响应体**。legacy 响应只加性补 `isSuccessful`/`cookieCount`。
- **代理票据 `POST /v1/sources/{sourceId}/proxy-token`**：TTL 600 秒（legacy 30 分钟保留），scope=`bookSourceProxy`、bsid 绑定单一源；令牌只能访问 `/reader3/bookSourceProxy` 前缀（中间件既有约束），主 JWT 不进 iframe URL。前端登录预览改用该端点，并用 v1 登录返回的 `previewUrl` 打开代理页。**代理转发/持久化 Cookie 时剔除 `reader_access`/`reader_refresh`**：代理页与主站同源，浏览器请求一定带本站会话 Cookie，原样转发等于把主 JWT 交给第三方书源；只保留上游站点自己的 Cookie。
- **导出 `GET /v1/sources:export`**：全量 `[BookSource]`（含规则），`Content-Disposition: attachment; filename="reader-sources-<UTC>.json"`、`Cache-Control: no-store`；前端导出直接下载服务端 attachment，不再拉全量 JSON 再本地拼文件。
- **换源落地 `PUT /v1/books/{bookId}/source`**：body `{targetBookId}`；只改绑登记行（bookId 不变），进度/分组/书签/正文历史保留，清旧源目录/详情/评论缓存并回填候选元数据。目标与当前相同 422；当前书未登记 404；当前书不在书架 409 `BOOK_NOT_IN_SHELF`；目标书不存在/无源 404；目标已在书架 409 `CONFLICT`。legacy `/reader3/setBookSource` 保留，内部共用 `change_book_source`。
- **前端接线**：`api/source.ts` 的登录、票据、文件解析、远端拉取、导出全部切 v1；`SourcesView` 登录 toast 使用服务端 message、预览用 `previewUrl`；`api/bookshelf.setBookSource` 切 PUT。`getBookSources` 全量读取封装删除，导出走 `exportBookSources` blob。


