# AGENTS.md

本文件是本仓库**工程事实的唯一权威**，面向 AI 编码代理与贡献者。任何与本文件冲突的说明（包括历史文档、`CLAUDE.md`、代码注释）以代码为准；发现不一致时，请修正文档而不是照抄文档。

---

## 项目定位

Reader-Rust 是 [阅读3.0](https://github.com/hectorqin/reader) 的 Rust 重写版：一个**书源阅读 API 服务端**，负责书源管理、内容抓取、规则解析、书架与用户数据持久化，并内置一套 Vue 3 Web 界面。

- 仓库：<https://github.com/givenge/reader-rust>
- 文档站：<https://givenge.github.io/reader-rust/>
- 免责声明：本项目只提供书源管理、解析、阅读与缓存的技术能力，**不内置、不存储、不分发任何受版权保护的书籍内容**。使用者需自行确保所添加的书源与访问内容已获合法授权。

---

## 常用命令

### 后端（Rust）

```bash
cargo run                      # 开发模式运行，默认监听 0.0.0.0:8080
cargo build                    # 调试构建
cargo build --release          # 发布构建
cargo test                     # 全部测试（Rust 侧共 230 个，另有 2 个真实网络用例默认忽略）
cargo test <关键字>             # 按名称过滤测试
cargo clippy --all-targets     # 静态检查
cargo fmt                      # 格式化
```

- 必须在**仓库根目录**运行：`storage/` 与 `.env` 都按相对路径解析。
- 首次 `cargo build` 约需 10 分钟以上，耗时集中在 `rquickjs`（JS 引擎）、`ring`、`libsqlite3-sys` 的 C 代码编译；之后增量构建只需数秒。

### 前端（Vue 3 + Vite）

```bash
cd frontend
npm install
npm run dev                    # 开发服务器，默认 http://localhost:5173
npm run build                  # 类型检查 + 构建 → frontend/dist/
npm run preview                # 预览构建产物
npm test                       # vitest 单元测试
```

- `npm run build` 会先执行 `vue-tsc -b`，**类型错误会直接导致构建失败**。
- 部署到子路径（nginx 反代 `https://example.org/read/`）时用
  `VITE_BASE_PATH=/read/ npm run build`；**新增任何绝对路径都要过
  `utils/appBase.ts`**（`API_BASE` 给接口/SSE，`withAppBase` 给服务端返回的
  `/reader3/...` 路径），否则子路径部署下会 404。
- 开发服务器把 `/reader3` 反向代理到后端（见 `frontend/vite.config.ts`）。代理目标端口必须与后端 `SERVER_PORT` 保持一致。
- 前端可用脚本只有 `dev` / `build` / `preview` / `test`，**没有 `lint` 或 `serve` 脚本**。

### 端到端测试（Playwright）

```bash
npm install                    # 仓库根目录，安装 @playwright/test
npx playwright install chrome

# 需要先手动启动后端
npm run test:e2e               # 等价于 playwright test
npm run test:e2e:headed        # 有头模式
npm run test:e2e:ui            # UI 模式
```

- 用例位于 `tests/e2e/*.spec.ts`，默认访问 `http://127.0.0.1:8080`。
- 后端换端口时用环境变量覆盖：`PLAYWRIGHT_BASE_URL=http://127.0.0.1:18080 npm run test:e2e`。
- 被测后端建议以 `RATE_LIMIT_DISABLED=true` 启动，否则登录失败限速会拦住重复跑的用例；登录用例还需要 `E2E_USERNAME`/`E2E_PASSWORD` 与后端 `ADMIN_USERNAME`/`ADMIN_PASSWORD` 一致。
- 配置使用系统已安装的 Chrome（`channel: 'chrome'`），机器上需要有 Chrome。

### 发布

统一走仓库脚本，**不要手动打 tag 或手工推镜像**：

```bash
./scripts/release.sh           # 自动在最新 tag 上递增 patch 并发布
./scripts/release.sh v1.0.9    # 发布指定版本
./scripts/release.sh --minor   # 递增次版本号
```

完整流程见 [维护者文档 · 发布](./docs/maintainers/release.md)。

---

## 配置

配置来源优先级：**环境变量 > `.env` 文件 > `src/app/config.rs` 中的代码默认值**。

```bash
cp .env.example .env
```

`.env` 已被 gitignore；后端即使没有 `.env` 也能用默认值启动。

| 变量 | 代码默认值 | 说明 |
|------|-----------|------|
| `SERVER_HOST` | `0.0.0.0` | 监听地址，本地开发建议改 `127.0.0.1` |
| `SERVER_PORT` | `8080` | 监听端口 |
| `DATABASE_URL` | `sqlite:storage/reader.db?mode=rwc` | SQLite 连接串，`mode=rwc` 表示不存在则创建 |
| `STORAGE_DIR` | `storage` | 运行期数据根目录 |
| `ASSETS_DIR` | `storage/assets` | 上传资源目录 |
| `WEB_ROOT` | `frontend/dist` | 前端静态文件目录 |
| `LOG_LEVEL` | `info` | `trace` / `debug` / `info` / `warn` / `error` |
| `REQUEST_TIMEOUT_SECS` | `15` | 抓取上游站点的超时时间 |
| `JWT_SECRET` | 空 | JWT 签名密钥；留空时自动生成并持久化到 `<STORAGE_DIR>/jwt_secret` |
| `JWT_TTL_SECS` | `604800`（7 天） | 令牌有效期 |
| `ADMIN_USERNAME` | `admin` | 唯一账号的用户名，首次启动建号时使用；只允许小写字母/数字/下划线 |
| `ADMIN_PASSWORD` | 空 | 唯一账号的密码；非空时每次启动强制覆盖（兼作忘记密码的找回通道），相同则 no-op；留空时首启随机生成并打印到启动日志 |
| `USER_BOOK_LIMIT` | `2000` | 书架上限，`0` 表示不限制 |
| `USER_LOCAL_BOOK_LIMIT` | `0` | 本地上传上限，`0` 表示不限制 |
| `CACHE_USER_LIMIT_BYTES` | `536870912` | 正文缓存上限；`0` 表示不限制 |
| `CACHE_COVER_LIMIT_BYTES` | `268435456` | 封面缓存目录上限；`0` 表示不限制 |
| `REVIEW_CACHE_TTL_SECS` | `604800`（7 天） | 章评/段评缓存有效期；`0` 表示不过期 |
| `REVIEW_CACHE_USER_LIMIT_BYTES` | `67108864` | 评论缓存上限；`0` 表示不限制 |
| `ALLOW_PRIVATE_NETWORK` | `true` | 出站请求是否允许访问私网/内网地址；自托管单用户默认放行（局域网书源是正常用法），公网部署应设为 `false` |
| `CORS_ALLOWED_ORIGINS` | 空 | 跨域来源白名单；留空仅同源 |
| `RATE_LIMIT_DISABLED` | `false` | 豁免登录限速（开发/测试用）。默认：IP 登录败 5 次封该 IP 登录 6h、用户名败 10 次封 6h。e2e 跑测试时建议后端开此项 |

两点需要注意：

- **配置结构是扁平的**，没有嵌套层级，因此不存在「用 `__` 表示层级」这种用法。
- `JWT_SECRET` 留空时后端会在启动阶段生成随机密钥写入 `storage/jwt_secret`，开发开箱即用；**多实例部署必须显式配置同一个值**，否则各实例签发的令牌互不认可。

---

## 代码结构

```
src/
  main.rs / lib.rs        入口，各十余行
  api/                    路由与 HTTP 处理（axum），18 文件约 7000 行
    router.rs             全部路由定义 + 鉴权分组（唯一真相来源）
    handlers/             14 个领域模块：book、book_source、user、bookmark、
                          book_group、ai_book、ai_model、ai_proxy、replace_rule、
                          image、webdav、cache（缓存清理与统计）、chapter_image、
                          review（章评/段评）；
                          另有共享的 multipart.rs（限量读取工具）
  auth/                   JWT 鉴权，4 文件
    jwt.rs                Claims 定义与 HS256 编解码
    secret.rs             JWT_SECRET 解析与持久化
    extractor.rs          CurrentUser / MaybeUser 提取器
    middleware.rs         require_auth / require_admin / optional_auth
  service/                业务编排，11 文件约 7100 行
    image_service.rs       图片管道：id 映射 + HEIC→JPEG + 永久缓存 + 回源自愈
  parser/                 规则解析引擎，6 文件约 4300 行
    rule_engine.rs        核心（3000 行）：六种用途的解析入口 + 评论解析
    rule_analyzer.rs      组合规则拆分（正确处理引号与括号嵌套）
    html.rs / jsonpath.rs / js.rs
  crawler/                reqwest 抓取与 URL 处理，5 文件约 1370 行
    url_analyzer.rs       占位符替换、页面选择、内联 JS
    url_guard.rs          出站请求守卫（SSRF 防护）
  model/                  BookSource 等数据结构，15 文件约 1000 行
  storage/               SQLite（sqlx）+ 文件缓存，8 文件约 800 行
    db/migrations/        仅 0001_init.sql（历史兼容补丁已并入）
    cache/file_cache.rs   章节内容文件缓存，以 MD5 命名；无 TTL，按容量淘汰
    cache/review_cache.rs 评论缓存；**有 TTL**（默认 7 天），按容量淘汰
  app/                    配置加载与启动引导
  error/                  错误类型
  util/                   加密、哈希、文本、时间等工具
frontend/                 Vue 3 + TypeScript + Vite + Pinia 前端
docs/                     VitePress 文档站，见文末「文档地图」
tests/                    Rust 集成测试（11 文件）+ Playwright e2e
scripts/release.sh        发布脚本
storage/                  运行期数据，gitignored，首次启动自动创建
```

规模参照（便于判断改动影响面）：后端 `src/` 共 82 个 `.rs`、约 26300 行，其中最大的三个文件是
`parser/rule_engine.rs`（约 4700 行）、`api/handlers/book.rs`（约 3100 行）、`service/book_service.rs`（约 3100 行）；
前端 `src/` 下 118 个文件（73 个 `.ts`，其中 21 个是测试；39 个 `.vue`；3 个 CSS + 2 个静态资源 + 1 个 JS 工具）。

---

## 请求链路

```
HTTP 请求
  → api/router.rs          路由匹配 + 鉴权分组（公开 / 可选 / 需登录 / 管理员）
  → auth/middleware.rs     校验 JWT，把身份写入请求扩展；失败即 401 / 403
  → api/handlers/*         从 CurrentUser 取身份与参数
  → service/*              业务编排（缓存命中判断、书源选择、请求头与 Cookie 准备）
  → parser/rule_engine.rs  按 BookSource 规则解析；需要页面内容时经 crawler 抓取
  → crawler/*              reqwest 抓取上游页面（占位符展开、字符集解码）
  → 统一响应包装返回 JSON
```

各层依赖方向（已核实）：`api → service → parser → crawler`，另有 `service → auth`（签发令牌用 `auth::jwt`）。
`parser/` 与 `crawler/` 都**不依赖** `service/`；
`parser` 与 `crawler` 之间是双向编译期依赖（解析经 crawler 抓取并过出站守卫；crawler 展开 URL 内联 JS 时回调 parser 的 JS 求值）；
`model/`、`util/`、`error/` 是被各层共用的底座模块；`storage/` 由 `service/` 使用。
`auth/` 是 HTTP 层的横向关注点：`service` 只用它的令牌编解码，中间件自己持有一条只读的身份查询，不反向依赖 service。

响应一律包装为：

```json
{ "isSuccess": true, "errorMsg": "", "data": {} }
```

- `isSuccess=false` 时从 `errorMsg` 读取失败原因。
- 未登录/令牌无效或过期 → **HTTP 401**，`errorMsg` 为 `"需要登录"`（用户可读文案，前端原样 toast）；前端凭 401 状态码弹出登录框。
- `/reader3` 是纯 API 命名空间：未注册路径返回 JSON 404，不落到静态文件服务。

鉴权说明：**单用户**，使用 **JWT（HS256）**。唯一账号由启动时的 bootstrap 创建：用户名取 `ADMIN_USERNAME`，密码取 `ADMIN_PASSWORD`（非空则每次启动强制覆盖，兼作找回通道），都为空则首启随机生成并打印到启动日志；不提供注册、用户管理与角色/权限分层——登录即拥有全部能力。登录返回的 `accessToken` 是标准 JWT，载荷为 `{ sub, ns, iat, exp, ver }`。传递方式只有两种：`Authorization: Bearer <jwt>` 头，或查询参数 `accessToken`（SSE 与 `<img>` 无法设置请求头，只能走查询串）。中间件位于 `src/auth/middleware.rs`，分 `require_auth` / `optional_auth` 两档，在 `api/router.rs` 里按分组挂载；handler 通过 `CurrentUser` 提取器取身份，不再自行解析凭据。撤销靠 `users.token_version`：改密码自增版本号即作废该账号所有旧令牌。

### 静态资源与 404

前端使用 **hash 路由**，深链接不依赖服务端回落，因此后端**不提供 SPA fallback**：

| 路径 | 行为 |
|------|------|
| `/` | 返回 `WEB_ROOT/index.html` |
| `/assets/*` | 先查 `WEB_ROOT/assets`，再回落到 `ASSETS_DIR` |
| `WEB_ROOT` 下的真实文件（`sw.js`、`site.webmanifest`、favicon、`icons/`、`svg/`） | 按文件名直接可取 |
| `/reader3/*` 未注册 | JSON 404（不落到静态服务） |
| 其他不存在的路径 | 404，**不会**回落 index.html |

新增前端根级静态文件时无需改路由；但若引入 SPA 深链接（切到 history 路由），需要重新加回落。

---

## 书源规则引擎

`RuleEngine` 的公开方法对应书源的六种用途，全部是**同步函数**（JS 求值通过 rquickjs 同步完成）：

`search_books` · `explore_books` · `book_info` · `chapter_list` · `content` · `next_content_url`

评论（章评 / 段评）另有一组方法：`chapter_reviews` · `para_review_index` · `para_reviews`，
外加 `chapter_review_url` / `para_review_index_url` / `para_review_url` 三个 URL 求值入口。
评论 URL 对**章节正文响应**求值（与 `nextContentUrl` 同理：书籍 ID / 章节 ID / 版本号都在正文响应里），
模板里额外可用 `{{page}}`、`{{count}}`、`{{paraIndex}}`、`{{bookUrl}}`、`{{chapterUrl}}`。
书源没声明 `ruleReview` / `ruleParaReview` 时接口返回 `enabled: false`，前端不渲染任何入口。
评论配图走 `imageRule`，按**列表规则**求值（`$.image_url[*]` / 多行 JS / `img@src`）；
地址按站点顺序原样返回，挑哪个格式渲染是客户端的事（番茄会同时给 HEIC 与 JPEG）。
排序参数 `sort`（`hot` 默认 / `time`）通过 `{{sort}}` 传给书源模板，并计入缓存键；
书源模板没用 `{{sort}}` 时响应里的 `serverSort` 为 false，前端据此说明「最新」只重排已加载的条目。
前端对「最新」**始终**做一次客户端重排兜底——上游对 `sort` 的支持并不完全可靠
（实测番茄书源部分章节的章评列表会忽略 sort 返回热度序），站点排对时这是无操作。
空的 `sort=` 参数在求值后自动剔除（FQWeb 对空串直接报参数错误）。
内联回复一律按时间升序返回（时间不是时间戳时保持站点顺序）。
**空页不进缓存**：上游偶发失败（限流、超时）与「真的没有评论」在响应上难以区分，
缓存空页等于把一次抖动放大成一整周「没有评论」。

章节配图有两条来源，完整规格见 [§17.7](./docs/reference/book-source-rules.md)：

- **正文 HTML 内嵌 `<img>`**（不需要规则）：书源的正文规则吐出 HTML 时图片原样保留，
  前端消毒后直接渲染。漫画/图集源、出版书插图、富文本正文（FQWeb 的 `/content/rich`）都走这条。
- **独立配图接口**（`ruleContentImage`）：`content_image_url` 对章节正文响应求值出配图接口地址，
  `content_images` 解析列表；字段规则都有默认值（`url` / `caption` / `para_index` / `width` / `height`），
  书源通常只写 `imageUrl` + `listRule`。`paraIndex` 是**正文行号**（插在该行之前），取不到位置的图排章末。
  书源没声明规则时接口返回 `enabled: false`；「这一章没有配图」是 `enabled: true` + 空列表。
  **配图不进缓存**：图片地址带时效签名（番茄的 `x-expires`），存下来过一阵就是死链。
  阅读器设置里的「章节配图」开关可整体隐藏（`ReadConfig.showChapterImages`）。

**图片地址一律收口到图片管道**（`service/image_service.rs`）：书籍封面（`coverUrl`）、
章节配图（`images[].url`）与评论配图（`items[].images`）在响应里都被换成
`/reader3/image/<id>`，前端不接触书源地址。评论配图的多个格式变体由后端挑一张能渲染的
（`pick_renderable`，HEIC 会被管道转成 JPEG），所以数组里只剩一个元素。
id = `md5(去掉查询串的地址)`；抓取失败时按登记时的书籍上下文回源刷新一次（封面走书籍详情，
章节/评论配图靠下一次拉取重新登记）。

### 解析方式识别

| 方式 | 识别规则 |
|------|---------|
| CSS 选择器 | 默认，用于 HTML（`.class`、`#id`、`tag`） |
| JSONPath | 自动识别 JSON（`$.data.list`） |
| XPath | 以 `/` 或 `./` 开头 |
| 正则 | 以 `:` 前缀书写（或显式 `@regex:`） |
| JavaScript | `js:` / `@js:` 前缀，或 `{{表达式}}` 内联 |

也可用显式前缀强制指定：`@css:`、`@json:`、`@xpath:`、`@regex:`。

### URL 占位符

书源中的 `searchUrl` / `exploreUrl` 支持：

- `{key}` — 搜索关键字
- `{page}` — 页码
- `{{表达式}}` — 内联 JavaScript
- `<1,2,3>` — 按页码取候选值
- 兼容旧写法的 `searchKey` / `searchPage`

**是 `{key}` 而不是 `${key}`。**

### 组合规则

多条规则可用分隔符组合，`rule_analyzer.rs` 在切分时会跳过引号与括号内部的同名符号，因此 `div[a="x&&y"]&&span` 不会被错误切开。需要「上一步结果作为下一步输入」的链式二次解析用 `@@`。

| 分隔符 | 语义 |
|--------|------|
| `&&` | 结果拼接（各条规则的命中结果依次合并） |
| `\|\|` | 取第一个非空结果 |
| `%%` | 并列取值 |

组合符在 CSS、XPath、**JsonPath**、**元素级字段**（列表项里的 `name`/`author` 等）与**列表正则**上都生效；
列表正则的 `&&` 是逐级下钻（前一段的完整匹配串喂给下一段，见规格 §9.3）。

字段级可用显式前缀强制指定解析方式：`@css:`、`@json:`、`@xpath:`、`@regex:`、`@js:`/`<js>`。
其中 `@regex:` 表示「不做匹配、直接用规则文本」（配合上游 `$n` 分组引用）。

### JS 规则

书源规则里的 `js:` / `@js:` / `<js>` 与 `{{表达式}}` 都在 rquickjs 沙箱里同步求值（内存/栈/超时都有上限）。

- `source` 是**真实书源对象**（`bookSourceUrl`、`bookSourceName`、`header` 等），`book`/`chapter` 目前只在 `formatJs` 里有真实对象。
- 规则脚本可以用顶层 `return`：首次求值报 `return not in a function` 时自动包成 IIFE 重试一次。
- `java.log` / `java.toast` / `java.openUrl` 是空操作；其余未实现的 `java.*`（`ajaxAll`、`connect`、`getCookie`、`webView` 等）会让整条规则抛错，清单见规格 §10。
- `<js>...</js>` 支持链式：`</js>` 之后的片段继续对 JS 结果求值（`@js:` 则吞掉后续整段）。

完整的规则语法与字段清单见 [`book-source-rules.md`](./docs/reference/book-source-rules.md)；面向书源作者的分篇教程见 [书源开发文档](./docs/book-source/index.md)。

---

## 数据与存储

- SQLite 通过 `sqlx` 访问，连接池 10 条连接，开启 WAL（`synchronous=NORMAL`），启动时自动执行 `src/storage/db/migrations/` 下的迁移。
- 主要表：`book_sources`（书源 JSON，按 `(user_ns, book_source_url)` 主键）、`users`、`json_documents`（通用 JSON 文档，按 namespace + name 存取）、`ai_book_memories`。迁移只有 `0001_init.sql` 一个。
- 章节正文以文件形式缓存于 `storage/cache/<ns>/<md5(bookUrl)>/`，文件名用 MD5，数据库里不存索引。
- 缓存**不按时间过期**，只在显式调用 `POST /reader3/purgeCache` 或超出容量上限时回收；占用可用 `GET /reader3/cacheStats` 查看。例外有二：评论缓存（见下）与书籍详情缓存（`storage/cache/bookinfo/`，10 分钟 TTL，`refresh=1` 强刷）。
- 唯一例外是**评论缓存**（`storage/cache/reviews/<ns>/<md5(bookUrl)>/`）：评论是会变的第三方数据，
  因此保留 7 天 TTL（`REVIEW_CACHE_TTL_SECS`），同时也受容量上限约束。
- **图片缓存**（`storage/cache/image/`）是书源图片的统一出口：`<id>.bin` 是图片本体、
  `<id>.json` 是映射记录（id = `md5(去掉查询串的地址)`，附上游地址与书籍上下文）。
  它**不按时间过期**（id 稳定，抓下来就长期复用），容量与封面共用 `CACHE_COVER_LIMIT_BYTES`；
  上游地址失效时按记录里的书籍上下文回源刷新一次再试（`service/image_service.rs`）。
- 另有若干**进程内**小缓存，重启即失效：书源正则编译缓存、JS `cache`/`kv` 与 jsLib 编译缓存、
  `exploreUrl` 的 JS 求值结果（按 `MD5(用户命名空间 | bookSourceUrl + exploreUrl)` 缓存 1 小时，
  键含用户维度——脚本输出可能含 `java.androidId` 等用户相关值）。
  它们都设了条目上限，超出即整表清空。
- `storage/` 全部属于运行期数据，**不要提交**，清理时也不要误删。

---

## 测试

Rust 侧共 **230 个测试**（161 个内联单元测试 + 69 个集成用例，另有 2 个 `#[ignore]` 的真实网络用例），分布为：

- `tests/` 下 14 个集成测试文件（69 个用例），其中 `book_source_compat.rs` 用例最多（17 个）；
  `auth_flow.rs`、`review_flow.rs`、`chapter_image_flow.rs` 与 `image_pipeline.rs` 起真实监听端口，
  前者覆盖 401、静态回落、缓存清理与改密吊销令牌，其余各用一个假上游覆盖评论规则（7 天缓存、
  按类型清理）、章节配图（配图规则、无图不报错、正文 HTML 内嵌图片透传）与图片管道
  （封面地址改写、HEIC→JPEG、过期签名回源自愈、缓存命中不重抓）；
- `src/` 内的内联单元测试模块（161 个）。

前端使用 vitest，共 22 个 `*.test.ts`（86 个用例）。

需要注意：

- `tests/yckceo_live_sources.rs` 会**访问真实网络**抓取在线书源，在无网或受限环境中失败属预期行为。
- `tests/e2e/` 的 Playwright 用例需要先手动启动后端。

新增功能时请补充测试：集成测试放 `tests/`，纯逻辑单元测试就近放在 `src` 的内联模块里。

---

## 开发约定

1. **改动要同步文档。** 新增或修改 `/reader3/*` 接口时，同时更新 `docs/api/` 下对应页面；改动配置项时更新本文件与 `docs/guide/configuration.md`。
2. **文档是开发目标的一部分。** 若某功能只在文档中描述、代码尚未实现，**不要删除文档条目**，按统一格式标注保留：

   ```
   > **未实现**：<说明>（依据：<代码位置>）
   ```

3. **不要提交** `storage/`、`.env`、构建产物（`target/`、`frontend/dist/`）。
4. **发布只走 `scripts/release.sh`**。脚本会自动同步 `Cargo.toml`、根 `package.json`、`frontend/package.json` 的版本号，并要求工作区干净（**包括没有未跟踪文件**）。
5. 修改前端后至少跑一次 `npm run build`，确保类型检查通过。
6. `AGENTS.md` 与 `CLAUDE.md` 保持单一来源：`CLAUDE.md` 只是导入入口，内容不要在这里重复维护。

## 前端 UI 约定

管理类弹窗/面板（设置抽屉、书源管理、缓存管理、WebDAV 备份、书源订阅等）共用一套形态语言，**新弹窗照此实现，不要发明新样式**：

- **桌面端居中卡片，移动端整屏**。断点全站统一 **767px**（顶栏汉堡、整屏菜单、各弹窗同一断点）。整屏时：隐藏遮罩（被完全盖住）、去圆角与边框、头部/底部加 `safe-area` 内边距。参考实现：`SettingsDrawer.vue`、`CacheLibraryModal.vue`、`WebdavManager.vue`、`SourceManager.vue`。
- **入场动画随形态切换**：用 `matchMedia('(max-width: 767px)')` 驱动一个 `isMobileLayout` ref，移动端 `slide-right`、桌面端 `scale`（`<Transition :name="...">`）。
- **头部操作区中心对齐**（`align-items: center`），不要顶对齐——按钮高度不一会视觉错位。
- **图标按钮无边框**（关闭、刷新等）：全局 `button` 已默认无边框，纯图标 + 悬停出底色（`--color-bg-hover`）即可；注意别让「带边框按钮」的共享选择器把它捎上。
- **toast 固定右下角**，容器带 safe-area 内边距 + `align-items: flex-end`（各条宽度独立，不跟随最长那条），单条上限 380px。
- **弹层遮罩全站统一**：背景与模糊一律用 `--overlay-mask-bg`（`rgba(0,0,0,0.45)`）+ `--overlay-mask-blur`（`blur(4px)`，需同时写 `-webkit-backdrop-filter`），不要发明新色值；唯一例外是图片灯箱（近黑 0.88）。移动端整屏形态下遮罩被面板完全盖住，样式仍照写。
- 移动端导航是**顶栏汉堡 + 整屏菜单**（`AppMobileMenu.vue`），底部胶囊已删除，不要恢复；深浅色开关统一用 `ThemeSwitch.vue`（顶栏与移动菜单共用，设置抽屉里不再放）。
- 阅读器内的评论超长折叠：正文默认 6 行 `line-clamp` 截断，渲染后量 `scrollHeight`/`clientHeight` 决定是否露出「展开」；「展开」跟在截断省略号后（零宽浮动占位 + `float: right`），展开后「折叠」在条目右上角与右下角。

---

## 文档地图

面向不同读者的文档分工如下，改文档前先确认应该改哪一份：

| 文档 | 读者 | 内容 |
|------|------|------|
| `README.md` | 访客 / 使用者 | 项目简介、特性、最快上手路径 |
| `AGENTS.md`（本文件） | AI 编码代理 / 贡献者 | 工程事实权威：命令、配置、结构、约定 |
| `docs/guide/` | 部署者 / 最终用户 | 安装部署、配置、功能说明、用户手册 |
| `docs/api/` | 接口集成方 | `/reader3/*` 接口参考 |
| `docs/book-source/` | 书源作者 | 书源规则编写教程 |
| `docs/reference/book-source-rules.md` | 书源作者 / 实现者 | 阅读3.0 规则完整兼容规格 |
| `docs/maintainers/` | 维护者 | 架构说明、发布流程 |
| `docs/archive/` | 维护者 | 历史设计与计划快照，**不代表当前实现** |
