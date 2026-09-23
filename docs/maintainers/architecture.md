# 架构说明

本文描述 Reader-Rust 的代码组织方式与关键设计取舍。配置项、命令等速查内容见仓库根目录的 `AGENTS.md`。

## 分层结构

```
frontend/  (Vue 3 + Vite + Pinia)
     │  HTTP /reader3/*   统一 { isSuccess, errorMsg, data } 包装
     ▼
src/api/          路由匹配、鉴权提取、参数解析、响应包装（axum）
     ▼
src/service/      业务编排：缓存命中、书源选择、请求头与 Cookie 准备、书架与用户规则、AI 资料
     ▼
src/parser/       按 BookSource 规则解析；需要页面内容时经 crawler 抓取
     ▼
src/crawler/      reqwest 抓取上游页面，URL 占位符展开、页面选择、字符集解码

底座（被各层按需引用，不反向依赖上层）：
  src/model/   数据结构        src/util/    加密 / 哈希 / 文本 / 时间
  src/error/   统一错误类型     src/storage/ 持久化（由 service 使用）
```

依赖方向已核实为单向：`api → service → parser → crawler`，且 `parser/` 与 `crawler/` **不依赖** `service/`。

::: tip 图里是「依赖方向」，不是「调用顺序」
绝大多数流程中，**抓取由 `service` 发起**，拿到响应体后再把字符串交给 `parser` 解析 —— 例如

```rust
let res = self.fetch_with_rate(source, spec).await?;      // service 经 crawler 抓取
let books = self.parser.search_books(source, &res.body, &res.url);  // parser 只解析
```

（见 `src/service/book_service.rs:259-265`）

`parser` 之所以也依赖 `crawler`，是因为少数规则需要它自己再发请求（例如顺着「下一页」规则继续抓取）。
所以「`parser` 依赖 `crawler`」说的是编译期依赖，不表示每次解析都走网络。
:::

## 模块职责

### `src/api/`

| 文件 | 职责 |
|------|------|
| `router.rs` | **路由真相来源**。全部业务路由挂在 `/reader3` 下，唯一例外是根路径 `GET /health` |
| `auth.rs` | 从请求头 `Authorization` 或查询参数 `accessToken` 提取凭证 |
| `mod.rs` | `AppState`：共享配置与各 service 实例 |
| `handlers/` | 按领域拆分，共 12 个模块：`book`、`book_source`、`book_group`、`bookmark`、`rss`、`user`、`ai_book`、`ai_model`、`ai_proxy`、`replace_rule`、`update`、`webdav` |

`handlers/book.rs` 是最大的单个文件（约 3200 行），涵盖书架、章节、缓存、上传等书籍相关接口。

静态资源由同一台 axum 实例托管：

- `/assets` → `WEB_ROOT/assets`，未命中则回落到 `ASSETS_DIR`
- 其余路径 → `WEB_ROOT`（fallback，用于前端路由）

请求体上限 100 MB（本地书籍与备份导入需要），并挂了 CORS、请求 ID 与 trace 中间件。

### `src/service/`

业务规则所在层，handler 里只做参数提取，判断逻辑都下沉到这里。

| 文件 | 职责 |
|------|------|
| `book_service.rs` | 搜索、发现、详情、目录、正文的编排（最大的 service） |
| `book_source_service.rs` | 书源 CRUD、导入导出、可用性检测 |
| `user_service.rs` | 用户、会话、token、权限 |
| `local_txt_book.rs` / `local_epub_book.rs` | 本地书籍导入与解析 |
| `ai_book_service.rs` | AI 资料生成与增量更新 |
| `ai_model_service.rs` | 后端模型配置与可见性控制 |
| `json_document_service.rs` | 通用 JSON 文档存取（namespace + name） |
| `book_group_service.rs` / `update_service.rs` | 分组、版本更新检查 |

### `src/parser/`

规则解析引擎，6 个文件合计约 4200 行。

| 文件 | 职责 |
|------|------|
| `rule_engine.rs` | 核心入口（2508 行）。公开方法对应书源的六种用途：`search_books`、`explore_books`、`book_info`、`chapter_list`、`content`、`next_content_url` |
| `rule_analyzer.rs` | 组合规则切分。按 `&&` / `\|\|` / `%%` 拆分，**会正确跳过引号与括号内的分隔符** |
| `html.rs` | CSS 选择器（`scraper`） |
| `jsonpath.rs` | JSONPath（`jsonpath_lib`） |
| `js.rs` | JavaScript 求值（`rquickjs`） |

`RuleEngine` 的方法都是**同步函数** —— JS 求值通过 rquickjs 同步完成。`BookService` 统一通过 `spawn_blocking` 把它们搬到 tokio 阻塞线程池执行，避免同步解析占住 async worker。

JS 求值本身带资源上限：内存 128MiB、调用栈 1MiB、单次求值 5 秒超时（中断处理器打断）、返回值 32MiB 上限；`java.*` 内部请求有独立 Cookie jar（按 `user_ns` 隔离）与 30 秒超时。

解析方式识别顺序与显式前缀见 [书源规则引擎](/book-source/rules)；完整兼容规格（含字段表与求值语义）见 [参考规格](/reference/book-source-rules)。

### `src/crawler/`

| 文件 | 职责 |
|------|------|
| `http_client.rs` | 按 `user_ns` 缓存独立 reqwest Client（各自独立 Cookie jar 与连接池，数量受 `USER_LIMIT` 约束），支持 gzip / brotli / deflate |
| `url_analyzer.rs` | URL 占位符展开（`{key}`、`{page}`、<code v-pre>{{js}}</code>、`<1,2,3>`）、分页生成、内联 JS |
| `url_guard.rs` | 出站请求守卫：协议 + 主机名 + DNS 解析后逐 IP 校验（拦截私网、环回、链路本地、云元数据地址），跟随重定向时逐跳校验；`ALLOW_PRIVATE_NETWORK` 控制开关 |
| `fetcher.rs` | 抓取重试与响应读取；`read_body_limited` 边收边计数，响应体超过 32MiB 直接拒绝，避免超大响应/解压炸弹打爆内存 |

### `src/storage/`

| 位置 | 职责 |
|------|------|
| `db/mod.rs` | sqlx 连接池（5 条连接），启动时自动跑迁移 |
| `db/migrations/` | 三个迁移：`0001_init`、`0002_add_user_ns`、`0003_users_and_account_documents` |
| `cache/file_cache.rs` | 章节正文文件缓存，文件名用 MD5，数据库只存索引 |
| `fs/` | `storage/` 与 `assets/` 的文件操作 |

### 其他

- `src/app/` —— 配置加载（`config.rs`）与启动引导
- `src/model/` —— `BookSource` 及各 rule 结构体，与书源 JSON 字段一一对应（大量 `#[serde(rename)]`）
- `src/error/` —— 统一错误类型
- `src/util/` —— 零散工具

## 一次搜索请求的完整链路

以 `GET /reader3/searchBook?key=xxx&bookSourceUrl=yyy` 为例：

1. `router.rs` 匹配到 `handlers::search_book`
2. handler 从 `AppState` 取出 `BookService`，提取 `key` 与目标书源
3. `book_service` 查找书源配置；`SECURE=true` 时先校验密钥
4. 书源 `searchUrl` 经 `url_analyzer` 展开占位符 → 得到真实 URL
5. `crawler` 发起请求（自动按响应头解码字符集）
6. `rule_engine.search_books` 按 `ruleSearch` 逐字段解析出书籍列表
7. 结果包装成 `{ isSuccess, errorMsg, data }` 返回

多书源搜索（`searchBookMultiSSE`）把第 2–7 步并发跑在多本书源上，并用 SSE 把每个书源的结果流式推给前端 —— 因此响应不是一次性 JSON，前端需要按 SSE 协议读取。

## 鉴权与身份

请求里的身份信息统一由 `src/api/auth.rs` 的 `AuthContext` 提取，共三个独立字段，各自支持「请求头」与「URL 查询参数」两种传法：

| 字段 | 请求头 | 查询参数 | 说明 |
|------|--------|---------|------|
| `access_token` | `Authorization` | `accessToken` | 用户登录凭证 |
| `secure_key` | `X-Secure-Key` | `secureKey` | 安全模式密钥，与用户无关 |
| `user_ns` | `X-User-NS` | `userNS` | 数据归属命名空间 |

### 登录凭证不是 JWT

登录成功后服务端生成**不透明 token**，以 `用户名:token` 的形式返回。`Authorization` 头既接受裸 token，也接受 `Bearer <token>` 前缀（大小写均可）—— 但这只是兼容写法，**token 本身不是 JWT，没有 payload、不能自解析**。

凭证的持久化方式是：

- `users.token` 保存该用户最近一次登录的 token
- `user_sessions` 表按 `(username, token)` 存多端会话，并带 `expire_at` 过期时间

因为按会话存表，同一账号可**多端同时登录**，且服务端可以逐会话失效。

校验失败时返回 HTTP 400 且 `errorMsg = "NEED_LOGIN"`（后端唯一的 HTTP 401 来自 WebDAV 的 Basic 认证，不走这套 JSON 结构），前端据此弹出登录框。

`SECURE=true` 是**另一层**独立机制，只校验 `secureKey`，与用户登录无关。两者可以叠加：安全模式挡机器访问，登录挡未授权用户。

### `user_ns` 的作用

`user_ns` 是**多用户数据隔离键**，不是用户表字段。`book_sources` 表用 `(user_ns, book_source_url)` 作复合主键 —— 每个命名空间持有自己的一整套书源，书架、AI 资料等也按 `user_ns` 隔离。默认命名空间是 `'default'`。

它还可以由客户端直接指定（不登录也能带），这为「同一实例服务多个独立用户组」提供了可能。

## 安全机制

书源由用户导入、抓取 URL 由查询参数传入，这些输入都不可信。主要防线：

| 机制 | 位置 | 说明 |
|------|------|------|
| 出站守卫（SSRF 防护） | `crawler/url_guard.rs` | 所有用户可控的出站请求统一校验：仅 http/https、拒绝私网/环回/链路本地/云元数据地址（含 DNS 解析后逐 IP 检查）、重定向逐跳复检（域名目标同样做 DNS 解析）。`ALLOW_PRIVATE_NETWORK` 可放行（默认跟随 `SECURE`） |
| 响应体上限 | `crawler/fetcher.rs` | 单次抓取响应体上限 32MiB，边收边计数；显式 Content-Length 超限直接拒绝；JS 侧 `java.*` 请求与 jsLib 远程拉取同上限 |
| JS 沙箱资源上限 | `parser/js.rs` | QuickJS Runtime 设内存（128MiB）/栈（1MiB）/执行时间（5s）上限；`java.*` 请求 30 秒超时、返回值 32MiB 上限 |
| 同步解析隔离 | `service/book_service.rs` | 规则解析（含 exploreUrl 的 `@js:`）全部走 `spawn_blocking`，第三方书源的 JS 死循环拖不垮 worker |
| Cookie jar 隔离 | `crawler/http_client.rs` | 按 `user_ns` 独立 Cookie jar 与连接池，避免用户间站点会话串号 |
| JS 状态隔离 | `parser/js.rs` | 书源 JS 的 `cache`/`kv` 键按 `user_ns` 加前缀，`java.*` 的 HTTP 客户端同样按用户池化 |
| XPath 表达式上限 | `parser/html.rs` | 长度 ≤8KiB、括号/谓词嵌套 ≤64 层：sxd-xpath 按嵌套递归，书源可控的深嵌套表达式会爆栈终止进程 |
| 上传/删除路径校验 | `api/handlers/user.rs`、`util/safe_path.rs` | 文件名白名单（含 Windows 保留设备名）+ 词法级路径解析，杜绝 `..` 穿越写删 `storage/` 之外的文件 |
| WebDAV 路径收敛 | `api/handlers/webdav.rs` | 相对路径按 `/`、`\` 双分隔符切分并拒绝 `..`/盘符/ADS/设备名；multipart 文件名经 `sanitize_file_name`；上传字段限量读取，下载流式返回 |
| 密码哈希 | `util/crypto.rs` | Argon2id（PHC 字符串自含盐与参数），哈希与校验在 `spawn_blocking` 中执行 |
| 登录限速 | `service/user_service.rs` | 同用户名 10 分钟窗口失败 8 次锁定 5 分钟；悲观计数（尝试先计数、成功再清除）使并发爆发无法绕过；WebDAV Basic 认证共享同一份限速 |
| CORS | `api/router.rs` | 默认仅同源（不下发任何 CORS 头）；跨域需 `CORS_ALLOWED_ORIGINS` 显式白名单 |
| 会话 token | `service/user_service.rs` | CSPRNG 直出 48 位（≈285 bit 熵），SQLite 按会话存储、可逐会话失效 |
| 封面代理 | `service/book_service.rs` | Content-Type 收敛为位图白名单（防 `text/html`/`svg` 经 `/cover` 的同源 XSS）；封面缓存目录 256MiB 容量上限、按最旧优先淘汰 |

文件缓存（`storage/cache`）有 7 天 TTL 与单用户 512MiB 容量上限，超限按最旧优先淘汰。

> 已知残余风险：出站守卫的 DNS 校验与 reqwest 实际连接是两次独立解析（TOCTOU），
> 控制权威 DNS 的攻击者理论上可用 rebinding 绕过；彻底封堵需要在连接层钉扎 IP，
> 当前 reqwest 未暴露该能力，暂以重定向逐跳复检缓解。

## 存储设计

SQLite 表：

| 表 | 主键 | 用途 |
|----|------|------|
| `book_sources` | `(user_ns, book_source_url)` | 书源配置（整份 JSON 存储） |
| `users` | `username` | 用户、token、权限开关（`is_admin`、`enable_webdav`、`enable_local_store`、`enable_ai_model`） |
| `user_sessions` | `(username, token)` | 多端会话与过期时间 |
| `json_documents` | `(namespace, name)` | 通用 JSON 文档存取（用户配置、书源变量等复用这张表） |
| `ai_book_memories` | `(user_ns, book_key)` | AI 资料 |

> `book_cache` / `chapter_cache` 两张表曾随 0001 迁移创建，但从未被代码读写
> （书籍元信息按需抓取、章节正文走文件缓存）——已由 0004 迁移删除。

设计取舍：

- **书源存整份 JSON** 而不是打散成列 —— 书源字段会随阅读3.0 规范演进，打散后每次都要改表；整份存换来结构灵活性，代价是无法按字段建索引。
- **章节正文存文件而不是数据库** —— 单章动辄数十 KB 且数量极多，塞进 SQLite 会让库文件迅速膨胀并拖慢备份。文件用 MD5 命名，天然去重。
- **`json_documents` 通用化** —— 用户配置、书源变量等零散键值不再各建一张表，减少迁移次数。
- **权限用布尔列而非角色表** —— 目前只有 `is_admin` 加三个功能开关（WebDAV / 本地存储 / AI 模型），用列更直观；若将来权限维度继续膨胀，需要再拆表。

`storage/` 整个目录属于运行期数据，已被 gitignore，清理时不要误删。

## 前端结构

```
frontend/src/
  views/       8 个路由页：Home / Reader / Explore / Recent / Rss / RssArticle / RssManage / AiBook
  components/  31 个组件，含 reader/ 与 source-manager/ 两个子目录
  stores/      Pinia：reader / bookshelf / explore / source / rss / aiBook / app
  api/         14 个接口模块 + http.ts（axios 实例）
  utils/       PWA、简繁转换、TTS、加密工具
```

两个关键约定：

- `api/http.ts` 里的 axios 实例 `baseURL = /reader3`，超时 120 秒。请求拦截器用 `utils/secureAccess` 的 `buildAuthHeaderValues(localStorage)` 取出凭证，分别注入 `Authorization` 与 `X-Secure-Key`。
- 响应拦截器会自动拆掉 `{ isSuccess, errorMsg, data }` 外壳，业务代码直接拿 `data`。识别到 `errorMsg === 'NEED_LOGIN'`、`data === 'NEED_LOGIN'` 或 HTTP 401 时派发 `need-login` 事件拉起登录框（同一时间 1.5 秒内只派发一次，避免并发请求弹出多个登录框）。

对返回裸数据（封面图、文件下载等没有 `isSuccess` 字段的响应）的接口，拦截器会原样放行。

## 扩展点

| 想做什么 | 动哪里 |
|---------|--------|
| 加一个接口 | `router.rs` 注册路由 + 对应 `handlers/<领域>.rs` 写 handler，业务逻辑放 `service/` |
| 改书源解析行为 | `parser/rule_engine.rs`；组合规则切分改动在 `rule_analyzer.rs` |
| 支持新的 URL 占位符 | `crawler/url_analyzer.rs` |
| 加一张表 | 在 `db/migrations/` 新增迁移文件，**不要改历史迁移** |
| 加配置项 | `app/config.rs` 的 `AppConfig` + `Default` + `set_default` 三处都要改，并同步更新 `AGENTS.md` 与 [配置](/guide/configuration) |
| 改前端接口封装 | `frontend/src/api/http.ts` 与对应模块 |
