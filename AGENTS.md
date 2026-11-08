# AGENTS.md

本文件记录仓库的工程约定，面向 AI 编码代理与贡献者。文档与代码冲突时以代码为准，发现不一致时修正文档。

本文件只写稳定的约定。默认值、文件清单、测试数量、路由全量列表这类容易变化的事实直接看源码，不在这里重复；需要数字时用对应命令现查。

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
cargo run                      # 开发模式运行，默认监听 127.0.0.1:8080（Docker 镜像显式设 0.0.0.0）
cargo build                    # 调试构建
cargo build --release          # 发布构建
cargo test                     # 全部测试（真实网络用例默认忽略）
cargo test <关键字>             # 按名称过滤测试
cargo clippy --all-targets     # 静态检查
cargo fmt                      # 格式化
```

- 必须在**仓库根目录**运行：`storage/` 与 `.env` 都按相对路径解析。
- 首次 `cargo build` 约需 10 分钟以上，耗时集中在 `rquickjs`（JS 引擎）、`ring`、`libsqlite3-sys` 的 C 代码编译；之后增量构建只需数秒。
- `.cargo/config.toml` 把并行编译限制为 4 路：单个 `rustc` 约占 1.8GB 内存，默认的 12 路并行会把 32GB 机器的提交内存推到上限。内存充足时用 `CARGO_BUILD_JOBS` 覆盖。
- 改任意一个源文件后 `cargo test` 都要重新编译整个 crate 两次（lib rlib 与 lib 测试 harness）并重链全部集成测试，一到两分钟；`cargo run` 的增量构建才是秒级。
- `cargo run` 运行时锁着 `target/debug/reader-rust.exe`，此时 `cargo test` 会在最后链接 bin 时失败（os error 5）。跑测试前先停掉开发服，或给测试单独指定 `--target-dir`。

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
  后端侧要同步设 `COOKIE_PATH=/read/reader3`（见配置表），否则会话 Cookie 的
  `Path` 不覆盖实际 API 地址，登录成功后所有接口仍 401。
- 开发服务器把 `/reader3` 反向代理到后端（见 `frontend/vite.config.ts`）。代理目标端口必须与后端 `SERVER_PORT` 保持一致。
- 前端可用脚本只有 `dev` / `build` / `preview` / `test`，**没有 `lint` 或 `serve` 脚本**。

### 端到端测试（Playwright）

```bash
npm install                    # 仓库根目录，安装 @playwright/test
npx playwright install chrome
cargo build                    # 零配置自起需要 target/debug 里的构建产物

npm run test:e2e               # 等价于 playwright test
npm run test:e2e:headed        # 有头模式
npm run test:e2e:ui            # UI 模式
```

- 用例位于 `tests/e2e/*.spec.ts`。**零配置**：不设任何环境变量时，globalSetup
  自动用 `target/debug/reader-rust.exe` 在 18081 起临时后端（独立临时
  storage + 数据库 + assets + 固定凭据 reader1/password123 + 豁免限速），
  跑完即清，CI 直接可用。
- 要跑自己的实例：设 `PLAYWRIGHT_BASE_URL=http://127.0.0.1:18080` 并同时给
  `E2E_USERNAME`/`E2E_PASSWORD`（与后端 `ADMIN_USERNAME`/`ADMIN_PASSWORD` 一致）。
  自起与外部实例二选一：设了 PLAYWRIGHT_BASE_URL 就必须给凭据。
- 本机 `target/debug` 被正在运行的 `cargo run` 占用（Windows 不允许覆盖 exe）时，
  用 `E2E_SERVER_BIN=target/verify/debug/reader-rust.exe` 指向独立构建产物。
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
`src/app/config.rs` 是变量名、默认值与解析规则的唯一来源，`.env.example` 是可直接复制的样例，完整说明见 [配置文档](./docs/guide/configuration.md)。本节只记录不看代码就容易踩的规则。

```bash
cp .env.example .env
```

- 配置结构是扁平的，没有嵌套层级，不存在用 `__` 表示层级的写法。
- `.env` 值含逗号或空格时必须用双引号包裹。dotenvy 遇到无法解析的行会中断加载，该行之后的变量全部静默失效，启动日志会对加载失败打 WARN。典型变量：`PRIVATE_NETWORK_WHITELIST`、`CORS_ALLOWED_ORIGINS`。
- `JWT_SECRET` 留空时会在启动阶段生成随机密钥并写入 `storage/jwt_secret`，开发环境可以直接用。多实例部署必须显式配置同一个值，否则各实例签发的令牌互不认可。
- 子路径部署时 `COOKIE_PATH` 必须与浏览器实际访问 API 的前缀一致。默认是 `/reader3`；nginx 剥掉 `/read/` 前缀时设为 `/read/reader3`，否则登录成功后所有接口仍返回 401。
- 出站请求默认拦截私网地址。书源或模型服务在局域网时，显式设 `ALLOW_PRIVATE_NETWORK=true` 整体放行，或用 `PRIVATE_NETWORK_WHITELIST` 精确放行。白名单为空时仍然拦截。
- 入站请求有处理超时，大上传与 AI 代理路由使用更长的独立超时。具体秒数、豁免范围与 `0` 值的处理都在 `src/app/config.rs`，调整时以代码为准。
- 登录限速默认按 IP 与用户名分别计数。服务挂在反向代理或 Docker NAT 后面时配置 `TRUSTED_PROXIES`，否则所有请求共用代理地址的失败次数，可能误封全体访问者。
- AI 与 sidecar 相关变量只在服务端维护，模型密钥从不下发到浏览器；`getAiModelConfig` 只返回各类型是否可用。sidecar 的启动方式、uv 依赖管理与模型调用预算见 `src/app/config.rs` 的字段说明与 [配置文档](./docs/guide/configuration.md)。

---

## 代码结构

```
src/
  main.rs / lib.rs        入口
  api/                    路由与 HTTP 处理（axum）
    router.rs             全部路由定义 + 鉴权分组（唯一真相来源）
    handlers/             领域模块：book/ 是目录（mod 共享层 resolve_book/ResolvedBook +
                          search/explore/toc/content/shelf/local_upload/sse/available_source
                          八个子模块）、book_source、user、bookmark、
                          book_group、ai_book、ai_model、ai_proxy、agent、replace_rule、
                          image、webdav、cache（缓存清理与统计）、chapter_image、
                          chapter_edit（用户版本、版本时间线、版本 diff 与选择、
                          单章/整本 TXT 导入预览、导入详情差异与批量写入——批量走
                          分块事务 + 章级保存点，导入可凭 previewId 复用预览缓存免二次上传；
                          大文件走 chapter_import_upload 的自适应分片，预览/导入凭 uploadId 复用）、
                          review（章评/段评）、search_cache（缓存内全文检索，只扫
                          已缓存章节、不触发上游请求）；另有共享的 multipart.rs（限量读取工具）。
                          webdav 的协议语义（PROPFIND/PUT/LOCK 等）由 dav-server
                          crate 实现，本模块只做 Basic 认证、按用户隔离目录与
                          Windows 路径加固
  auth/                   JWT 鉴权
    jwt.rs                Claims 定义与 HS256 编解码
    secret.rs             JWT_SECRET 解析与持久化
    extractor.rs          CurrentUser / MaybeUser 提取器
    middleware.rs         require_auth / optional_auth
  service/                业务编排
    book_service/         书籍服务主壳（目录：mod 结构体/构造/共享夹具 + fetch 抓取限速地基 +
                          registry 登记与 Cookie + explore 搜索/发现/详情 + toc 目录抓取与章节登记表 +
                          content 正文回源 + review 评论/配图 + cache 清理统计 +
                          shelf 书架读写 + cached_search 缓存内全文检索）
    chapter_content_service.rs 正文池与版本时间线：sha256 去重、zstd 压缩、版本追加、
                          最新指针、每用户版本选择、24h 回源判定、批量分块事务写入
    image_service.rs       图片管道：id 映射 + HEIC→JPEG + 永久缓存 + 回源自愈
                          + ?w= 按宽度派生缓存 + Accept 协商有损 webp（load_variant）
    chapter_diff_service.rs 章节版本 diff：进程内 imara-diff（git 同款 Histogram 算法）
                          + 变更块相似度配对 + 结果缓存
    refresh_token_service.rs Refresh Token 轮换：SHA-256 摘要落库、一次性消费、30 秒宽限、
                          重放吊销 family
    agent_sidecar_service.rs AI 资料编排：spawn Python sidecar、逐章 run/result 泵、
                          工具应答（统一内容链路）、单章看门狗、任务注册表与取消
  parser/                 规则解析引擎
    rule_engine/          核心（目录：kernel 字段求值内核与模板插值 + search/info/toc/
                          content/review/image 六个用途子模块，测试随簇同迁）
    rule_analyzer.rs      组合规则拆分（正确处理引号与括号嵌套）
    html.rs / jsonpath.rs / js.rs（js.rs 含求值计数/耗时/预算耗尽的诊断埋点）
  crawler/                reqwest 抓取与 URL 处理
    url_analyzer.rs       占位符替换、页面选择、内联 JS
    url_guard.rs          出站请求守卫（SSRF 防护）
  model/                  BookSource 等数据结构
  storage/               SQLite（sqlx）+ 文件缓存
    db/migrations/        0001_init.sql（基础表结构、书籍/书源登记表、正文池与版本时间线）
                          + 0002（书签/替换规则实体表）+ 0004（book_groups 分组实体表）
                          + 0005（refresh_tokens 轮换状态）+ 0006（chapter_title_overrides 章节标题覆盖）
                          + 0007（chapters 章节登记表与 book_toc_state 目录状态）；详见 docs/maintainers/database.md
    db/content_repo.rs    正文池/时间线/最新指针的数据访问
    db/chapters_repo.rs   章节登记表（目录持久层）的数据访问
    cache/file_cache.rs   目录型缓存的容量控制与占用统计（评论、图片等）
    cache/review_cache.rs 评论缓存；**有 TTL**（默认 7 天），按容量淘汰
  app/                    配置加载与启动引导
  error/                  错误类型
  util/                   加密、哈希、文本、时间等工具；keyed_mutex.rs 分键互斥锁
                          （图片加载与正文回源的单飞收口，满员逐出跳过在用锁）
frontend/                 Vue 3 + TypeScript + Vite + Pinia 前端
docs/                     VitePress 文档站，见文末「文档地图」
sidecar/                  AI 资料编排 Python sidecar（stdio NDJSON；依赖由 uv 自动管理，见配置文档）
  agent_sidecar/          agent 循环、提示词、patch 归一化合并、模型客户端
  tests/                  pytest，与前端展示层共用口径的领域规则在此逐字移植
tests/                    Rust 集成测试 + Playwright e2e
scripts/release.sh        发布脚本
storage/                  运行期数据，gitignored，首次启动自动创建
```

前端按职责拆分：`views/ReaderView.vue` 只做模板与编排接线，横翻分页、连续滚动、位置恢复、搜索、选字、TTS、自动播放、手势各自成 composable；纯计算放在 `utils/`。store 层以 `stores/reader.ts` 为核心（当前书、目录、正文状态），另有单向依赖的 readerContent、readerExtras、readerMarks、readerConfig 四个子 store 与 search；子 store 可以读核心，核心不反向 import。阅读器目录使用 virtua 虚拟滚动，书源管理与缓存管理列表按页渲染（`components/common/ListPager.vue` 与 `utils/listPager.ts`）。

---

## 请求链路

```
HTTP 请求
  → api/router.rs          路由匹配 + 鉴权分组（公开 / 可选 / 需登录 / 管理员）
  → auth/middleware.rs     校验 JWT，把身份写入请求扩展；失败即 401 / 403
  → api/handlers/*         从 CurrentUser 取身份与参数
  → service/*              业务编排（正文版本解析与回源、缓存命中判断、书源选择、Cookie 准备）
  → parser/rule_engine.rs  按 BookSource 规则解析；需要页面内容时经 crawler 抓取
  → crawler/*              reqwest 抓取上游页面（占位符展开、字符集解码）
  → 统一响应包装返回 JSON
```

各层依赖方向（已核实）：`api → service → parser → crawler`，另有 `service → auth`（签发令牌用 `auth::jwt`）。
`parser/` 与 `crawler/` 都**不依赖** `service/`；
`parser` 与 `crawler` 之间是双向编译期依赖（解析经 crawler 抓取并过出站守卫；crawler 展开 URL 内联 JS 时回调 parser 的 JS 求值）；
`model/`、`util/`、`error/` 是被各层共用的基础模块；`storage/` 由 `service/` 使用。
`auth/` 是 HTTP 层的横向关注点：`service` 只用它的令牌编解码，中间件自己持有一条只读的身份查询，不反向依赖 service。

响应一律包装为：

```json
{ "isSuccess": true, "errorMsg": "", "data": {} }
```

- `isSuccess=false` 时从 `errorMsg` 读取失败原因。
- 未登录/令牌无效或过期 → **HTTP 401**，`errorMsg` 为 `"需要登录"`（用户可读文案，前端原样 toast）；前端凭 401 状态码弹出登录框。
- 上游站点失败（403/404/限流/超时）→ **HTTP 502** + 带真实状态码的 `errorMsg`
  （loginCheckJs 先跑、仍非 2xx 才在 service 层统一透传；网络层错误只给泛化文案不带 URL）。
  正文 2xx 但解析为空：书源声明了配图规则按纯图章节放行，否则 404「正文为空」。
- `/reader3` 是纯 API 命名空间：未注册路径返回 JSON 404，不落到静态文件服务。
- `/reader3/getChapterList` 的成功响应带强 ETag（章节列表序列化的 md5）与
  `Cache-Control: private, no-cache`；请求头 `If-None-Match` 命中时直接回 **304** 空 body
  （宽松比较：逗号列表、`W/` 弱前缀、`*`），浏览器复用本地缓存。
- `/reader3/getBookContents`（POST，需登录）是**预加载用批量正文接口**：一次 1..=10 章、
  并发上限 4，与单章 `getBookContent` 共用同一条读取链路；每章独立成败
  （`data`/`error` 二选一），整体恒 200，章节数或 `chapterUrl` 缺失整批 400。
  前端整批失败（断网/401/400）时回退全部目标章逐章预加载；批量成功时**失败章
  （单章 `error` 或响应缺失）逐章兜底**——走单章接口重试并自带浏览器缓存兜底，
  成功章不重复请求；响应期间换书则旧结果直接丢弃、不回退。
- 单章与批量正文都接受 `knownVersionId`（调用方本地缓存的版本号）：与当前解析
  版本一致时响应 `unchanged: true`、`content` 为空串（版本信息/diff 照常返回），
  前端用浏览器缓存的正文补上；`refresh=1` 忽略。浏览器缓存（IndexedDB）记录因此
  带版本号，**编辑章与普通章一样进缓存**——会不会串版本由比对兜底，不再靠
  「编辑章不缓存」；离线读章直接用缓存版本，恢复在线后第一次读取自动比对换新。
- `/reader3/aiProxyImage` 目前**无前端调用方**（AI 资料的地图图片改由后端直接下载落盘到
  `ASSETS_DIR`），端点保留备用。

鉴权说明：**单用户**，采用 **Access Token + Refresh Token** 双令牌。唯一账号由启动时的 bootstrap 创建：用户名取 `ADMIN_USERNAME`，密码取 `ADMIN_PASSWORD`（非空则每次启动强制覆盖，兼作找回通道），都为空则首启随机生成并打印到启动 stderr（不写入滚动日志文件）；不提供注册、用户管理与角色/权限分层——登录即拥有全部能力。登录、刷新、改密成功时服务端设置两枚 HttpOnly Cookie：`reader_access`（HS256 JWT，默认 6 小时，载荷 `{ sub, uid, iat, exp, ver }`；`sub` 是用户名，仅登录与展示；`uid` 是自增身份 id，存储命名空间一律用 `user_<uid>`）与 `reader_refresh`（`rt_` + 32 字节随机串，默认 7 天滑动续期，数据库只存 SHA-256 摘要）。两枚都是 `HttpOnly; SameSite=Strict`、host-only；`Path` 由 `COOKIE_PATH` 配置（默认 `/reader3`，子路径部署需改成浏览器实际前缀如 `/read/reader3`），`Secure` 由 `COOKIE_SECURE`（`auto`/`true`/`false`）决定。浏览器的业务请求、SSE 与 `<img>` 都由浏览器自动携带 Cookie，前端不再从 localStorage 读写令牌；localStorage 只留非敏感 `reader-auth-hint`（用户名 + Access Token 过期时间）用于路由守卫、离线 UI 与提前续期。JSON 响应仍保留 `accessToken` 供 API 客户端用 `Authorization: Bearer`，但不再返回 `refreshToken`。取令牌顺序：`Authorization` 头 → `reader_access` Cookie → 查询参数 `accessToken`；查询串只允许 `bookSourceProxy` / `bookSourceClientLog` 的 30 分钟限定令牌，其他路径不再接受。Refresh Token 只随 `reader_refresh` Cookie 发给 `/reader3/refreshToken`（body 里的 `refreshToken` 字段不受理）。刷新是**一次性轮换**：换取成功即消费旧令牌并下发新的一对；已消费令牌有 30 秒宽限窗口（断网重试/双标签页并发），窗口外重放视为泄露并吊销整条登录会话（family）。中间件位于 `src/auth/middleware.rs`，分 `require_auth` / `optional_auth` 两档，在 `api/router.rs` 里按分组挂载；handler 通过 `CurrentUser` 提取器取身份，不再自行解析凭据。撤销分两层：改密码自增 `users.token_version` 并吊销全部 Refresh Token（旧 Access Token 立即失效）；登出清两枚 Cookie 并吊销当前 Refresh family，当前 Access Token 最迟 6 小时后自然过期。书源登录预览先用 `POST /reader3/mintBookSourceProxyToken` 换 30 分钟限定令牌（带 `scope`/`bsid`、只能访问代理路径、绑定单个书源 id），主 JWT 不进 iframe URL；代理 HTML 响应带 `Content-Security-Policy: sandbox allow-scripts allow-forms`，直接打开或嵌入都进入不透明源。前端在 `utils/authSession.ts` 里凭 Cookie 做单飞刷新（Web Locks/软锁协调多标签页）、到期前 5 分钟主动续期。

### 静态资源与 404

前端使用 **hash 路由**，深链接不依赖服务端回落，因此后端**不提供 SPA fallback**：

| 路径                                                                              | 行为                                                                     |
| --------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `/`                                                                               | 返回 `WEB_ROOT/index.html`                                               |
| `/assets/*`                                                                       | 先查 `WEB_ROOT/assets`，再回落到 `ASSETS_DIR`；一年 immutable            |
| `/fonts/*`                                                                        | `WEB_ROOT/fonts` 下的自托管字体分片（文件名带内容 hash）；一年 immutable |
| `WEB_ROOT` 下的真实文件（`sw.js`、`site.webmanifest`、favicon、`icons/`、`svg/`） | 按文件名直接可取；no-cache 每次重验证                                    |
| `/reader3/*` 未注册                                                               | JSON 404（不落到静态服务）                                               |
| 其他不存在的路径                                                                  | 404，**不会**回落 index.html                                             |

新增前端根级静态文件时无需改路由；但若引入 SPA 深链接（切到 history 路由），需要重新加回落。

---

## 书源规则引擎

`RuleEngine` 的公开方法对应书源的六种用途，全部是**同步函数**（JS 求值通过 rquickjs 同步完成）：

`search_books` · `explore_books` · `book_info` · `chapter_list` · `content` · `next_content_url`

评论（章评 / 段评）另有一组方法：`chapter_reviews` · `para_review_index` · `para_reviews`，
外加 `chapter_review_url` / `para_review_index_url` / `para_review_url` 三个 URL 求值入口。
评论 URL 对**章节正文响应**求值（与 `nextContentUrl` 同理：书籍 ID / 章节 ID / 版本号都在正文响应里），
模板里额外可用 `{{page}}`、`{{count}}`、`{{paraIndex}}`、`{{bookUrl}}`、`{{chapterUrl}}`、`{{variant}}`。
`variant` 是客户端当前显示的正文版本（`edited` 默认 / `upstream`），三个评论接口都接受并计入缓存键；
评论内容通常与版本无关，它主要影响段评在编辑版正文上的定位。
书源没声明 `ruleReview` / `ruleParaReview` 时接口返回 `enabled: false`，前端不渲染任何入口。
评论配图走 `imageRule`，按**列表规则**求值（`$.image_url[*]` / 多行 JS / `img@src`）；
地址按站点顺序原样返回，挑哪个格式渲染是客户端的事（番茄会同时给 HEIC 与 JPEG）。
排序参数 `sort`（`hot` 默认 / `time`）通过 `{{sort}}` 传给书源模板，并计入缓存键；
书源模板没用 `{{sort}}` 时响应里的 `serverSort` 为 false，前端据此说明「最新」只重排已加载的条目。
前端对「最新」**始终**做一次客户端重排作为后备——上游对 `sort` 的支持并不完全可靠
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
章节/评论配图靠下一次拉取重新登记）。取图支持 `?w=<像素宽>` 按宽度派生缓存（clamp 64..=2048、
只降不升，Triangle 滤镜）与 `Accept` 头协商的有损 webp（质量 80，否则 JPEG 质量 85）：
派生文件 `<id>.w<宽>.<webp|jpg>` 与原图同目录、共用容量记账，响应带 `Vary: Accept`；
派生图随原图重抓（`fetch_and_store`）与按书删除（`remove_by_book_id`）一并清理，
之后变体请求按需重建（命中逻辑不做 mtime 校验）；
GIF/WebP/AVIF/SVG/ICO 原样透传（不破坏动画与矢量），不带 `w` 也不要 webp 时原图直出。
派生图解码有 16384 单边/256MiB 上限（`decode_with_limits`），超限与解码失败一样回退原图，不 500。
前端按渲染场景给取图地址带宽度档位（`api/bookshelf.ts` 的 `getCoverUrl(coverUrl, width)` 与
`withImageWidth(url, width)`，后者同时认 `/reader3/image/` 与 `/reader3/cover?` 两种本站地址）：
封面列表 320、书籍详情 640、章节配图 1200、评论配图 480；宽度只是提示，后端只降不升；
本地 epub 资源（`/reader3/localEpubAsset`）与第三方直链一律不加参数。

### 解析方式识别

| 方式       | 识别规则                                  |
| ---------- | ----------------------------------------- |
| CSS 选择器 | 默认，用于 HTML（`.class`、`#id`、`tag`） |
| JSONPath   | 自动识别 JSON（`$.data.list`）            |
| XPath      | 以 `/` 或 `./` 开头                       |
| 正则       | 以 `:` 前缀书写（或显式 `@regex:`）       |
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

| 分隔符 | 语义                                   |
| ------ | -------------------------------------- |
| `&&`   | 结果拼接（各条规则的命中结果依次合并） |
| `\|\|` | 取第一个非空结果                       |
| `%%`   | 并列取值                               |

组合符在 CSS、XPath、**JsonPath**、**元素级字段**（列表项里的 `name`/`author` 等）与**列表正则**上都生效；
JsonPath 方言是 **RFC 9535**（jsonpath-rfc9535 crate；`=~` 请改用 `match()`/`search()`）；
XPath 有资源上限（长度 8KB / 嵌套 64 层 / 总步数 100、`//` 50），超限拒绝求值（防解析爆栈与 `//a` 链求值爆炸）。
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
- 主要表：`book_sources`（书源 JSON，整部署共享，`book_source_id INTEGER PRIMARY KEY AUTOINCREMENT` + `book_source_url` 唯一索引；**`book_source_id` 是书源的身份键**，删除后不复用，导入/订阅按 URL 去重时保号）、`books`（**书籍登记表**，整部署共享：`book_id INTEGER PRIMARY KEY AUTOINCREMENT` + `book_url` 唯一索引；对外一律 `book_<n>` 文本，repo 边界转换，绑定 `book_source_id`，换源只改绑不改 id；本地书 source_id 为 NULL；登记行不随清缓存删除）、`chapters`（**章节登记表**，0007，整部署共享：`(book_id, chapter_index)` 主键 + `(book_id, url) WHERE url != ''` 部分唯一索引；index 的持久载体，服务端可只凭 (bookId, index) 定位章节；换源整表替换，用户级清缓存不动它）、`book_toc_state`（目录抓取状态，complete + fetched_at，完整 1h/未完成 60s TTL）、`shelf_books`（**书架**，`(user_ns, book_id)` 主键，全列展开进度/分组/排序/元数据快照，读取时 JOIN books 注入 bookUrl/origin 等绑定字段）、`users`（`uid` 主键、用户数据命名空间锚点，命名空间一律 `user_<uid>`；username 仅登录与展示，唯一索引）、`json_documents`（`(user_ns, name)` 主键，通用键值；userdata 另有 `getUserdataBatch`/`saveUserdataBatch` 批量读写接口，单次 ≤20 个文档、任一条目非法整批 400 不落库、批量写走单事务任一失败整体回滚）、`bookmarks`（书签/划线实体表，`(user_ns, book_id, chapter_index, chapter_pos, kind)` 主键——kind 区分章书签 bookmark / 划线 underline，`chapter_pos` 是万分比章内进度 0–10000、章书签恒 0；删书级联 `DELETE WHERE book_id`）、`replace_rules`（替换规则实体表）、`ai_book_memories`（`(user_ns, book_id)` 主键，`book_url` 只作数据快照）、`book_groups`（书架分组实体表：`(user_ns, group_id)` 主键；**group_id 是位域、分配 2 的幂**（1/2/4/8…），与阅读3.0 的位掩码约定一致）。逐字段说明见 docs/maintainers/database.md。
- 前端 userdata 写入统一收口到 `frontend/src/utils/userdataQueue.ts` 的批量写队列：按文档名合并（同名后写覆盖前写）、200ms 防抖合成一次 `saveUserdataBatch`、单飞在途、失败按退避重试至多 3 次；成功清理按版本号判断是否被新写覆盖（每次入队分配递增版本，落地回调只删版本未变的条目——阅读统计入队的是同一活对象并原地修改，按引用相等会把在途期间的新值误删）；`pagehide` 与 `visibilitychange=hidden` 时用裸 fetch + `keepalive` 补发最后一次批量写（URL 走 `API_BASE`，不经 axios），同样按版本清理；登出前先 `flushUserdataQueue()`。阅读统计、最近阅读、已读标记三条写路径都经此队列。
- 前端 userdata 首屏读取走批量预读：文档名常量收在 `frontend/src/utils/userdataDocs.ts`（reading-stats / recent-books / readChapterHistory，避免 app.ts 直接 import store 造成循环依赖）；`stores/app.ts` 在登录成功（`setUser`）与 `bootstrapAuth` 确认已登录后 fire-and-forget 调 `api/userdata.prefetchUserdata`，一次 `getUserdataBatch` 把三份文档写入 30s 一次性缓存（`getUserdataBatch` 命中即删、只对 miss/过期条目发请求，避免长期陈旧）；写成功（`saveUserdataBatch`）与登出（`clearUserdataPrefetch`，由 `clearUser` 调用）使对应缓存失效；缓存 miss、过期或预读失败都回落 `getUserdataBatch` 批量 GET。前端 userdata 读写已全部走批量接口；服务端单文档 `getUserdata`/`saveUserdata` 端点保留兼容、前端不再调用。
- 书源列表在前端有一份 localStorage 冷启动快照（`reader-source-snapshot-v1`，24h TTL、最多 1000 条）：只存名称/链接/分组/启用状态等非敏感字段，**不落 header/loginUrl/jsLib/任何规则字段**。`stores/source.ts` 首屏先用快照渲染（`partial` 标记）再后台拉全量替换；书源编辑/批量写前一律先 `ensureFullSources()` 拿到全量对象，避免用残缺快照对象保存把规则清空。批量导入（`saveSources`）落库成功后的整表重拉是 **best-effort**：重拉失败只置 `partial`（本地列表暂缺新导入项，由下一次 `ensureFullSources` 补全），不把已成功的导入报成失败。
- 书源分组字段的拆分以后端 `split_source_groups`（`src/service/book_source_service.rs`）为唯一权威：分隔符仅半角逗号/分号、全角分号、顿号（全角逗号、竖线、斜杠都不是分隔符）；前端 `splitBookSourceGroups` 保持同口径、不得更宽，否则「删除失效书源」的本地移除会与后端实际删除范围不一致。
- **正文版本表（整部署共享，不带 `user_ns`）**：`chapter_contents`（正文池，`content_hash` 唯一，只插入不更新；`codec=1` 为 zstd）、`chapter_versions`（版本时间线，`kind` 为 upstream/edited/imported，含 `base_version_id` 与书源/章节快照，只插入不更新）、`chapter_latest`（最新上游/用户版本指针与回源时间，唯一允许更新的正文表）、`chapter_view_state`（`(user_ns, book_id, chapter_url)` 主键，每用户选中的版本；删除行表示跟随最新）。阅读页的 diff **始终以最新上游正文为基准**（评论源只对最新上游取段号，diff 标记要和它对齐）；`base_version_id` 只记录编辑时基于哪一版，不参与阅读页 diff。
- **身份模型**：请求的身份参数只认 `bookId`（书籍）与 `bookSourceId`（书源）；`bookUrl`/`bookSourceUrl`/`origin` 只是数据字段（爬虫取数、导入去重），不参与「同 id 同书」校验。书籍与书源 id 都是 AUTOINCREMENT、删除后不复用，正文历史不会串书。搜索/发现产出即登记（响应必带 bookId）；书籍级接口经 `resolve_book`（handlers/book/mod.rs）由 bookId 解析出当前绑定的 bookUrl + 书源。升级存量部署（beta 之前）：删除 `storage/` 后重启重建。
- **正文存储**：章节正文已从文件缓存迁到 SQLite 正文池；阅读默认版本按「用户选择 > 最近用户版本 > 最新上游」解析。读取时距上次回源尝试超过 `CONTENT_REVALIDATE_TTL_SECS` 会在后台回源，失败继续返回缓存并标记 `stale`。`refresh=1` 与历史面板的“检查更新”同步回源，失败也回退缓存。正文历史**严格保留，不自动淘汰，也不提供清理入口**。文本源返回空正文按失败处理，不写 0 字节上游版本；只有 `bookSourceType=2` 的图片源允许空正文，且只更新检查时间。存量空上游版本可用 `docs/maintainers/database.md` 的维护 SQL 手工清理。
- 目录已迁 SQLite 章节登记表（`chapters` + `book_toc_state`，0007 迁移）：完整目录 1h TTL、首屏分页的未完成目录只复用 60s、分页达到 `MAX_TOC_PAGES` 上限或整页重复熔断标未完成——语义与旧文件缓存一致，文件缓存层已删除；`resolve_chapter_by_index` 是 (bookId, index) 定位章节的解析入口；书籍详情缓存 `cache/bookinfo/<ns>/<bookId>.json`（1h TTL，`refresh=1` 强刷）；换源候选 `cache/<ns>/source_candidates/<bookId>.json`（1h TTL）；评论缓存 `cache/reviews/...`；图片缓存 `cache/image/...`。`data/` 只剩用户资产：`local_books` 与 `userConfig.json`（用户配置备份快照）。
- 可再生缓存**大多不按时间过期**，只在显式调用 `POST /reader3/purgeCache` 或超出容量上限时回收；占用可用 `GET /reader3/cacheStats` 查看。**正文历史不在清理范围内**（`purgeCache` 没有 content 类型）；`scope=book` 清目录/详情/换源候选/评论并连带该书的图片；`scope=user` 清该用户的全部可再生缓存，但不再删除书籍登记行，也不动章节登记表（与登记行同为整部署共享的主数据）。
- **评论缓存**（`storage/cache/reviews/<ns>/<bookId>/`）是会变的第三方数据，
  保留 7 天 TTL（`REVIEW_CACHE_TTL_SECS`），同时也受容量上限约束。
- **图片缓存**（`storage/cache/image/`）是书源图片的统一出口：`<id>.bin` 是图片本体、
  `<id>.json` 是映射记录（id = `md5(去掉查询串的地址)`，含上游地址与所属 `book_id`），
  `<id>.w<宽>.<webp|jpg>` 是按宽度/webp 协商的派生图（`?w=`，详见「图片管道」段）。
  它**不按时间过期**（id 稳定，抓下来就长期复用），容量与封面共用 `CACHE_COVER_LIMIT_BYTES`；
  上游地址失效时按 `book_id` 解析当前绑定回源刷新一次再试（`service/image_service.rs`）。
- 另有若干**进程内**小缓存，重启即失效：书源正则编译缓存、JS `cache`/`kv` 与 jsLib 编译缓存、
  `exploreUrl` 的 JS 求值结果（按 `MD5(用户命名空间 | bookSourceUrl + exploreUrl)` 缓存 1 小时，
  键含用户维度——脚本输出可能含 `java.androidId` 等用户相关值）。
  它们都设了条目上限，超出即整表清空。
- 书签按 `(bookId, chapterIndex, chapterPos, kind)` 判等（一书多条；章书签与划线分型共存，
  `chapterPos` 万分比章内进度）；替换规则 scope 用
  `book:<bookId>` / `source:<bookSourceId>`；换源 `setBookSource {bookId, targetBookId}`
  只改登记表绑定，进度/书签/AI 记忆/正文历史天然保留。
- 日志按天滚动落盘到 `storage/logs/reader-rust.log.YYYY-MM-DD`（stdout + 文件双写，
  文件不含 ANSI 颜色码；sidecar 的 stderr 经主进程 tracing 也落同一个文件）。
- `storage/` 全部属于运行期数据，**不要提交**，清理时也不要误删。

---

## 测试

`cargo test` 跑 Rust 单元测试与集成测试，当前用例数看输出即可，不在文档里维护。

- 纯逻辑的单元测试就近放在 `src` 的内联 `#[cfg(test)]` 模块里。
- 跨层行为放 `tests/`，用假上游覆盖 HTTP、存储、缓存与导入链路。鉴权、评论、章节配图、图片管道、WebDAV、批量正文与 AI sidecar 都有对应的集成测试。
- `agent_sidecar_flow` 使用 Node 假 sidecar，机器上需要有 Node。
- 默认忽略的用例会访问真实网络（在线书源），无网或受限环境中失败属预期行为。
- `tests/e2e/` 的 Playwright 用例零配置可跑（globalSetup 自起临时后端，见「常用命令」）。
- sidecar 使用 pytest，含从旧前端实现移植的领域规则用例与编排用例。

新增功能时请补充测试：集成测试放 `tests/`，纯逻辑单元测试就近放在 `src` 的内联模块里。

---

## 开发约定

1. **文档只写稳定的行为。** 用户可见的接口语义、配置语义或操作流程变了，才更新 `docs/api/`、`docs/guide/` 的对应页面。重命名、拆文件、内部实现调整不需要改文档。
2. **别把易变的事实写进本文件。** 默认值、文件清单、测试数量、路由全量列表这类内容以源码为准；本文件保留入口和容易踩的规则即可，避免一次改动要同步多处。
3. **文档是开发目标的一部分。** 若某功能只在文档中描述、代码尚未实现，**不要删除文档条目**，按统一格式标注保留：

   ```
   > **未实现**：<说明>（依据：<代码位置>）
   ```

4. **不要提交** `storage/`、`.env`、构建产物（`target/`、`frontend/dist/`）。
5. **发布只走 `scripts/release.sh`**。脚本会自动同步 `Cargo.toml`、根 `package.json`、`frontend/package.json` 的版本号，并要求工作区干净（**包括没有未跟踪文件**）。
6. 修改前端后至少跑一次 `npm run build`，确保类型检查通过。
7. `AGENTS.md` 与 `CLAUDE.md` 保持单一来源：`CLAUDE.md` 只是导入入口，内容不要在这里重复维护。
8. 合并前在本地跑 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 与 `cargo test`；格式化问题不要留到推送后再补。

## 前端 UI 约定

管理类弹窗/面板（设置抽屉、缓存管理、AI 设置、书源订阅等）共用一套形态语言，**新弹窗照此实现，不要发明新样式**：

- **弹窗 or 页面怎么选**：会被阅读器内打开的（AI 设置）必须是弹窗，保持挂载在 `App.vue` 并用 `defineAsyncComponent` 懒加载；只在设置抽屉有入口的重管理功能（书源管理 `/sources`、WebDAV 备份 `/webdav`）做成**独立路由页面**——可深链接、浏览器返回可用，且不进首包。页面式管理屏参考 `views/SourcesView.vue` / `views/WebdavView.vue`（页头左侧返回箭头 + 最大宽度容器，无遮罩/卡片壳）。
- **桌面端居中卡片，移动端整屏**。断点全站统一 **767px**（顶栏汉堡、整屏菜单、各弹窗同一断点）。整屏时：隐藏遮罩（被完全盖住）、去圆角与边框、头部/底部加 `safe-area` 内边距。参考实现：`SettingsDrawer.vue`、`CacheLibraryModal.vue`、`AiSettingsModal.vue`。
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

| 文档                                  | 读者                 | 内容                                   |
| ------------------------------------- | -------------------- | -------------------------------------- |
| `README.md`                           | 访客 / 使用者        | 项目简介、特性、最快上手路径           |
| `AGENTS.md`（本文件）                 | AI 编码代理 / 贡献者 | 工程约定：命令、配置要点、结构、开发约定 |
| `docs/guide/`                         | 部署者 / 最终用户    | 安装部署、配置、功能说明、用户手册     |
| `docs/api/`                           | 接口集成方           | `/reader3/*` 接口参考                  |
| `docs/book-source/`                   | 书源作者             | 书源规则编写教程                       |
| `docs/reference/book-source-rules.md` | 书源作者 / 实现者    | 阅读3.0 规则完整兼容规格               |
| `docs/maintainers/`                   | 维护者               | 架构说明、数据库、数据存储结构、发布流程 |
| `docs/archive/`                       | 维护者               | 历史设计与计划快照，**不代表当前实现** |
