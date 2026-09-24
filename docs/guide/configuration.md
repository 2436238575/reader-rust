# 配置

Reader-Rust 通过**环境变量**配置，支持从 `.env` 文件读取。代码默认值定义在 `src/app/config.rs`。

配置优先级：

```
环境变量  >  .env 文件  >  src/app/config.rs 中的代码默认值
```

## 完整配置项

### 服务器

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `SERVER_HOST` | `0.0.0.0` | 监听地址。只在本机使用时建议改为 `127.0.0.1` |
| `SERVER_PORT` | `8080` | 监听端口 |
| `REQUEST_TIMEOUT_SECS` | `15` | 抓取上游站点的 HTTP 超时时间（秒） |

### 存储

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `DATABASE_URL` | `sqlite:storage/reader.db?mode=rwc` | SQLite 连接串，`mode=rwc` 表示文件不存在时创建 |
| `STORAGE_DIR` | `storage` | 运行期数据根目录，章节缓存等文件写在这里 |
| `ASSETS_DIR` | `storage/assets` | 上传资源（图片、本地书籍等）目录 |
| `WEB_ROOT` | `frontend/dist` | 前端静态文件目录，由后端直接托管 |

### 日志

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `LOG_LEVEL` | `info` | `trace` / `debug` / `info` / `warn` / `error` |

### 鉴权

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `JWT_SECRET` | 空 | JWT 签名密钥。**留空时启动阶段生成随机密钥并写入 `<STORAGE_DIR>/jwt_secret`**，重启后已签发的令牌继续有效。多实例部署必须显式配置同一个值，否则各实例签发的令牌互不认可 |
| `JWT_TTL_SECS` | `604800`（7 天） | 令牌有效期。改密码/重置密码会立即作废该用户的所有令牌，与有效期无关 |
| `INVITE_CODE` | 空 | 注册邀请码，为空表示注册不校验邀请码 |
| `ALLOW_PRIVATE_NETWORK` | `true` | 是否允许服务端出站请求访问私网/环回/链路本地地址（`10.x`、`192.168.x`、`127.0.0.1`、`169.254.169.254` 等）。自托管单用户场景下，局域网书源、本地书源服务（如 `http://192.168.x.x:9999`）、本地模型服务都属正常用法，参考实现（阅读/Legado）同样不做限制。**多用户或公网暴露的部署请设为 `false`**，否则任意用户都能把服务端当成内网探测代理 |
| `CORS_ALLOWED_ORIGINS` | 空 | 允许跨域访问的来源白名单（逗号分隔）。留空表示**仅同源**——不发送任何 CORS 响应头；仅在前后端分离且前端独立域名部署时配置 |

鉴权基于 JWT，**没有「安全模式」开关**：除 `/health`、`/reader3/login`、`/reader3/getUserInfo`、`/reader3/logout` 与 WebDAV 的 Basic 认证入口外，所有 `/reader3/*` 接口都要求有效令牌，否则返回 401；管理员接口对非管理员返回 403。

令牌传法（详见 [API 概述 · 认证](../api/index#认证)）：

- 请求头：`Authorization: Bearer <jwt>`
- URL 查询参数：`?accessToken=<jwt>`（SSE 与 `<img>` 无法设置请求头时使用）

::: tip 生产部署请显式配置 `JWT_SECRET`
留空虽然能开箱即用，但密钥会写在 `storage/jwt_secret` 里。多实例或容器化部署（每次重建容器都换一份 storage）必须显式配置，否则重启即全体登出。
:::

### 用户配额

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `USER_LIMIT` | `50` | 用户总数上限 |
| `USER_BOOK_LIMIT` | `2000` | 单用户书架书籍数上限，`0` 表示不限制 |
| `USER_LOCAL_BOOK_LIMIT` | `0` | 单用户本地上传书籍数上限，`0` 表示不限制 |

### 缓存

缓存**不按时间过期**，只在显式调用 [`purgeCache`](../api/cache) 或超出容量时回收，因此容量上限是唯一的自动回收手段。`0` 表示不限制。

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `CACHE_USER_LIMIT_BYTES` | `536870912`（512 MiB） | 单用户章节正文缓存上限，超出时按修改时间最旧优先淘汰 |
| `CACHE_COVER_LIMIT_BYTES` | `268435456`（256 MiB） | 封面缓存目录上限 |

## 配置格式

配置项是**扁平的**，没有嵌套层级 —— 直接写大写的变量名即可：

```bash
SERVER_PORT=3000
DATABASE_URL=sqlite:custom/path/reader.db?mode=rwc
LOG_LEVEL=debug
```

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
JWT_TTL_SECS=604800
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

**生产部署**（固定签名密钥并开启邀请码）

```ini
SERVER_HOST=0.0.0.0
SERVER_PORT=8080
LOG_LEVEL=info
JWT_SECRET=<足够随机的密钥>
INVITE_CODE=<注册邀请码>
```

生产环境通常在前面挂一层 Nginx 做 TLS 终止与反向代理，见 [手动部署](./manual-deploy)。
