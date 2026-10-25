# 配置

Reader-Rust 通过**环境变量**配置，支持从 `.env` 文件读取。代码默认值定义在 `src/app/config.rs`。

配置优先级：

```
环境变量  >  .env 文件  >  src/app/config.rs 中的代码默认值
```

## 完整配置项

### 服务器

| 变量                   | 默认值    | 说明                                         |
| ---------------------- | --------- | -------------------------------------------- |
| `SERVER_HOST`          | `127.0.0.1` | 监听地址。对外提供服务（含 Docker，镜像已显式设为 `0.0.0.0`）时设为 `0.0.0.0` |
| `SERVER_PORT`          | `8080`    | 监听端口                                     |
| `REQUEST_TIMEOUT_SECS` | `15`      | 抓取上游站点的 HTTP 超时时间（秒）           |

### 存储

| 变量           | 默认值                              | 说明                                           |
| -------------- | ----------------------------------- | ---------------------------------------------- |
| `DATABASE_URL` | `sqlite:storage/reader.db?mode=rwc` | SQLite 连接串，`mode=rwc` 表示文件不存在时创建 |
| `STORAGE_DIR`  | `storage`                           | 运行期数据根目录，章节缓存等文件写在这里       |
| `ASSETS_DIR`   | `storage/assets`                    | 上传资源（图片、本地书籍等）目录               |
| `WEB_ROOT`     | `frontend/dist`                     | 前端静态文件目录，由后端直接托管               |

### 日志

| 变量        | 默认值 | 说明                                          |
| ----------- | ------ | --------------------------------------------- |
| `LOG_LEVEL` | `info` | `trace` / `debug` / `info` / `warn` / `error` 日志同时落盘到 `storage/logs/`（按天滚动，sidecar 输出合并进同一文件） |

### 鉴权

| 变量                        | 默认值           | 说明                                                                                                                                                                                                                                                                                                                                                                       |
| --------------------------- | ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `JWT_SECRET`                | 空               | JWT 签名密钥。**留空时启动阶段生成随机密钥并写入 `<STORAGE_DIR>/jwt_secret`**，重启后已签发的令牌继续有效。多实例部署必须显式配置同一个值，否则各实例签发的令牌互不认可                                                                                                                                                                                                    |
| `ACCESS_TOKEN_TTL_SECS`     | `21600`（6 小时） | Access Token（HS256 JWT）有效期。过期后前端用 Refresh Token 静默换新，用户不会感知到固定时长的重新登录                                                                                                                                                                                                                                                                      |
| `REFRESH_TOKEN_TTL_SECS`    | `604800`（7 天）  | Refresh Token 有效期，**滑动续期**：每次换取都签发新令牌并重新计时，闲置超过该时长才失效。客户端超过 30 秒宽限窗口后重放已消费令牌会吊销整条会话                                                                                                                                                                                                                              |
| `JWT_TTL_SECS`              | 空（已废弃）      | 旧版单令牌方案的有效期。未显式配置 `ACCESS_TOKEN_TTL_SECS` 时作为其兼容别名生效并打印启动警告；两者都配置时以新变量为准                                                                                                                                                                                                                                                      |
| `RATE_LIMIT_DISABLED`       | `false`          | 豁免登录失败限速（开发与测试环境用）。默认规则：IP 登录失败 5 次封该 IP 登录 6h；用户名登录失败 10 次封该用户名登录 6h。限速为进程内状态，多实例部署各自计数                                                                                                                                                                                                               |
| `TRUSTED_PROXIES`           | 空               | 登录限速的受信代理列表（逗号分隔，IP 或 CIDR，如 `"172.17.0.1, 172.17.0.0/16"`）。生产部署挂在 nginx/Caddy/Docker NAT 后面时，所有请求的对端地址都是代理 IP；攻击者连续输错 5 次密码，会使全体访问者被封禁 6 小时。配置后，只有对端地址命中受信列表时才解析 `X-Forwarded-For` 还原真实客户端地址（从右往左找第一个不在受信列表里的地址；格式非法或全部受信时回退到直连地址）。**留空（默认）不信任任何代理头**，一律按直连地址记账。值含逗号时必须用双引号包裹 |
| `ALLOW_PRIVATE_NETWORK`     | `false`          | 是否允许服务端出站请求访问私网/环回/链路本地地址（`10.x`、`192.168.x`、`127.0.0.1`、`169.254.169.254` 等）。默认 `false`（安全优先）：公网暴露的部署开箱即不会被打成内网探测代理。书源/模型服务在局域网时（如 `http://192.168.x.x:9999`），显式设为 `true` 整体放行，或保持 `false` 用 `PRIVATE_NETWORK_WHITELIST` 精确放行 |
| `PRIVATE_NETWORK_WHITELIST` | 空               | 私网白名单（逗号分隔），仅在 `ALLOW_PRIVATE_NETWORK=false` 时生效。条目支持 IP（`192.168.100.99`）、网段（`192.168.100.0/24`）、域名（`nas.lan`），均可带端口（`192.168.100.99:9999`，带端口时目标端口也须一致）。**名单为空时一律拦截**（不等同 `ALLOW_PRIVATE_NETWORK=true`）；名单非空时只有命中的目标可以出站，其余私网/环回地址一律拦截 |
| `CORS_ALLOWED_ORIGINS`      | 空               | 允许跨域访问的来源白名单（逗号分隔）。留空表示**仅同源**——不发送任何 CORS 响应头；仅在前后端分离且前端独立域名部署时配置                                                                                                                                                                                                                                                   |

鉴权使用 **Access Token + Refresh Token**：Access Token 是 6 小时的 HS256 JWT，Refresh Token 是 7 天的一次性随机串（数据库只存摘要）。**没有「安全模式」开关**：除 `/health`、`/reader3/login`、`/reader3/refreshToken`、`/reader3/getUserInfo`、`/reader3/logout` 与 WebDAV 的 Basic 认证入口外，所有 `/reader3/*` 接口都要求有效令牌，否则返回 401。

令牌传法（详见 [API 概述 · 认证](../api/index#认证)）：

- 请求头：`Authorization: Bearer <jwt>`（仅 Access Token）
- URL 查询参数：`?accessToken=<jwt>`（SSE 与 `<img>` 无法设置请求头时使用）
- Refresh Token 只放在 `POST /reader3/refreshToken` 的 JSON body 里，**永远不进 URL**

::: tip 生产部署请显式配置 `JWT_SECRET`
留空虽然能开箱即用，但密钥会写在 `storage/jwt_secret` 里。多实例或容器化部署（每次重建容器都换一份 storage）必须显式配置，否则重启即全体登出。
:::

### 账号（单用户）

不提供注册接口，唯一账号由服务端启动时创建（bootstrap）：

| 变量             | 默认值  | 说明                                                                                                                                                                                                   |
| ---------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `ADMIN_USERNAME` | `admin` | 唯一账号的用户名，首次启动建号时使用；仅允许小写字母、数字与下划线。建号后修改它相当于换一个全新账号（旧命名空间下的数据不可见）                                                                       |
| `ADMIN_PASSWORD` | 空      | 唯一账号的密码。**非空时每次启动都强制写入**（覆盖现有密码并使旧令牌失效），因此它也是忘记密码时的找回通道；与现密码相同时是 no-op，不会重置登录态。留空则首次启动随机生成 20 位强密码并打印到启动 stderr（不写入滚动日志文件）。**登录失败限速封禁期间，即使密码正确也会被拒绝**；忘记密码或被锁定时，设置该变量后重启即可强制重置并解锁 |

### 后端 AI 模型（可选）

AI资料/AI 地图/听书 TTS 可以使用「后端」模型：请求经 `/reader3/aiProxy` 由服务端转发，
**配置只存在于服务端，从不下发到浏览器**（前端只能看到各类型的可用状态布尔值，
浏览器全程接触不到 API Key）。三组各自独立，按 `AI_<类型>_ENABLED=true` 启用：

| 变量前缀     | 用途                     | 变量                                                                                                          |
| ------------ | ------------------------ | ------------------------------------------------------------------------------------------------------------- |
| `AI_TEXT_`   | 文本模型（AI资料生成）   | `ENABLED`、`BASE_URL`、`API_KEY`、`MODEL`、`USE_FULL_URL`                                                     |
| `AI_IMAGE_`  | 图片模型（AI 地图）      | `ENABLED`、`BASE_URL`、`API_KEY`、`MODEL`（默认 `gpt-image-1`）、`SIZE`（默认 `1024x1024`）、`USE_FULL_URL`   |
| `AI_SPEECH_` | 语音模型（听书 TTS）     | `ENABLED`、`BASE_URL`、`API_KEY`、`MODEL`（默认 `gpt-4o-mini-tts`）、`VOICE`（默认 `alloy`）、`FORMAT`（默认 `mp3`）、`USE_FULL_URL` |

示例：`AI_SPEECH_ENABLED=true` + `AI_SPEECH_BASE_URL=http://localhost:8825` + `AI_SPEECH_API_KEY=...`
即可让前端「听书 · 模型来源 = 后端」可用。

### AI 资料编排（Python sidecar）

AI 资料的使用建议、剧情摘要、人物/关系/地点与世界观的生成循环运行在**后端拉起的
Python 进程**里（Rust 只负责提交任务、转发进度与落库）。宿主机要求：

- Python >= 3.11（Docker 镜像已内置）；
- 仓库 `sidecar/` 目录随二进制一同部署（可执行文件旁的 `sidecar/` 会被自动识别）。

| 变量                                | 默认值                     | 说明                                                                                                     |
| ----------------------------------- | -------------------------- | -------------------------------------------------------------------------------------------------------- |
| `AGENT_SIDECAR_ENABLED`             | `true`                     | 是否启用 AI 资料编排；关闭后 AI 资料更新接口返回明确错误                                                 |
| `AGENT_SIDECAR_COMMAND`             | `python -m agent_sidecar`  | sidecar 启动命令（按空白切分，支持双引号包裹含空格的路径），进程工作目录为 `sidecar/`                    |
| `AGENT_SIDECAR_CHAPTER_TIMEOUT_SECS`| `1800`                     | 单章处理超时；超时即终止 sidecar 进程并中止批量任务（sidecar 内模型调用的总预算为 5×300 秒，与此值对齐；错误会带回真实原因，不会被截成超时）                                                      |
| `AGENT_SIDECAR_UV`                  | 空                        | uv 二进制路径；非空/PATH 可探测时由 uv 自动管理依赖（推荐），都没有时降级为 AGENT_SIDECAR_COMMAND      |

部署注意：

- **推荐 uv**：宿主机装好 [uv](https://docs.astral.sh/uv/)（Windows 为
  `winget install astral-sh.uv` 或官方脚本）后**无需任何额外操作**——后端自动探测
  PATH 里的 uv，用 `uv run` 拉起 sidecar，按 `sidecar/pyproject.toml` 自动建
  `sidecar/.venv` 并安装固定版本的依赖，系统 Python 完全不沾。
  `AGENT_SIDECAR_UV` 可显式指定 uv 二进制路径。
- **备选（手动 pip）**：没有 uv 时降级为直接跑 `python -m agent_sidecar`，需要宿主机
  已安装 Python >=3.11 并执行 `pip install httpx==0.28.1 pydantic==2.11.7`（版本
  以 `pyproject.toml` 为准）；Windows 上 `python` 可能命中 Microsoft Store 占位符，
  把 `AGENT_SIDECAR_COMMAND` 配成绝对路径即可。自定义 `AGENT_SIDECAR_COMMAND`
  会抑制 uv 模式（逃生门）。
- **Docker 镜像**已内置 uv 并预热依赖，开箱即用。
- **可用性探测**：后端启动时按解析出的启动方式做一次握手自检，结果通过
  `getAiModelConfig` 的 `agentReady` 暴露给前端；失败只打 warn，不影响主服务。

### 资源限额

| 变量                    | 默认值 | 说明                               |
| ----------------------- | ------ | ---------------------------------- |
| `USER_BOOK_LIMIT`       | `2000` | 书架书籍数上限，`0` 表示不限制     |
| `USER_LOCAL_BOOK_LIMIT` | `0`    | 本地上传书籍数上限，`0` 表示不限制 |

### 缓存

章节正文和版本历史存在数据库里，**严格保留、不自动淘汰，也不提供清理入口**。读取某章时距上次回源尝试超过回源间隔，会在后台回源一次；失败继续返回缓存。其余可再生缓存不按时间过期，只在显式调用 [`purgeCache`](../api/cache) 或超出容量时回收，`0` 表示不限制。

| 变量                            | 默认值                 | 说明                                           |
| ------------------------------- | ---------------------- | ---------------------------------------------- |
| `CONTENT_REVALIDATE_TTL_SECS`   | `86400`（24 小时）     | 正文回源间隔；`0` 表示只在强刷时回源           |
| `CACHE_COVER_LIMIT_BYTES`       | `268435456`（256 MiB） | 图片缓存目录上限（封面、章节配图、评论配图共用） |
| `REVIEW_CACHE_TTL_SECS`         | `604800`（7 天）       | 章评 / 段评缓存有效期，`0` 表示不过期          |
| `REVIEW_CACHE_USER_LIMIT_BYTES` | `67108864`（64 MiB）   | 评论缓存上限                                   |

## 配置格式

配置项是**扁平的**，没有嵌套层级 —— 直接写大写的变量名即可：

```bash
SERVER_PORT=3000
DATABASE_URL=sqlite:custom/path/reader.db?mode=rwc
LOG_LEVEL=debug
```

::: warning 值含特殊字符时必须加双引号

`.env` 用 dotenvy 解析：值里含**逗号、空格**等特殊字符（典型如
`PRIVATE_NETWORK_WHITELIST`、`CORS_ALLOWED_ORIGINS`）时必须用双引号包裹，如
`PRIVATE_NETWORK_WHITELIST="192.168.1.1:9999, 192.168.1.0/24"`。不加引号时
dotenvy 会在该行**中断加载**——之前的变量正常、之后的所有变量静默失效，
且大多有代码默认值作为后备，很难察觉（后端启动日志会对加载失败打 WARN）。

:::

::: warning 不存在 `__` 层级分隔符
配置加载使用 `config` crate 的默认环境变量源（无前缀），因此 `APP__SERVER__PORT` 这类写法**不会生效**，也不存在「用双下划线表示层级」的语义。
:::

## 使用 `.env` 文件

```bash
cp .env.example .env
```

```ini
SERVER_HOST=0.0.0.0
SERVER_PORT=8080
DATABASE_URL=sqlite:storage/reader.db?mode=rwc
WEB_ROOT=frontend/dist
LOG_LEVEL=info
ACCESS_TOKEN_TTL_SECS=21600
REFRESH_TOKEN_TTL_SECS=604800
```

`.env` 已被 gitignore，不会被提交。后端即使没有 `.env` 文件也能用代码默认值正常启动。

## 典型场景

**本地开发**（打开调试日志、只监听回环地址）

```ini
SERVER_HOST=127.0.0.1
SERVER_PORT=18080
LOG_LEVEL=debug
# JWT_SECRET 留空即可，密钥会自动生成到 storage/jwt_secret
```

**生产部署**（固定签名密钥与账号密码）

```ini
SERVER_HOST=0.0.0.0
SERVER_PORT=8080
LOG_LEVEL=info
JWT_SECRET=<足够随机的密钥>
ADMIN_USERNAME=admin
ADMIN_PASSWORD=<强密码>
```

生产环境通常在前面挂一层 Nginx 做 TLS 终止与反向代理，见 [手动部署](./manual-deploy)。
