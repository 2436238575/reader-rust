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

### 安全

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `SECURE` | `false` | 安全模式开关 |
| `SECURE_KEY` | 空 | 安全模式密钥 |
| `INVITE_CODE` | 空 | 注册邀请码，为空表示注册不校验邀请码 |

开启 `SECURE` 后，请求必须携带密钥，两种传法等价：

- 请求头：`X-Secure-Key: <SECURE_KEY>`
- URL 查询参数：`?secureKey=<SECURE_KEY>`

::: tip 为什么 `.env.example` 里写的是 `SECURE=true`
代码默认是 `false`，而模板给的是 `true` —— 这是刻意为之：模板按**生产安全默认**提供，代码默认按**开箱即用**提供。不要为了让两者一致而去改模板。
:::

### 用户配额

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `USER_LIMIT` | `50` | 用户总数上限 |
| `USER_BOOK_LIMIT` | `2000` | 单用户书架书籍数上限 |
| `USER_LOCAL_BOOK_LIMIT` | `0` | 单用户本地上传书籍数上限，`0` 表示不限制 |

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
SECURE=true
SECURE_KEY=your-secret-key
```

`.env` 已被 gitignore，不会被提交。后端即使没有 `.env` 文件也能用代码默认值正常启动。

## 典型场景

**本地开发**（打开调试日志、只监听回环地址、关闭安全模式）

```ini
SERVER_HOST=127.0.0.1
SERVER_PORT=18080
LOG_LEVEL=debug
SECURE=false
```

**生产部署**（开启安全模式与邀请码）

```ini
SERVER_HOST=0.0.0.0
SERVER_PORT=8080
LOG_LEVEL=info
SECURE=true
SECURE_KEY=<足够随机的密钥>
INVITE_CODE=<注册邀请码>
```

生产环境通常在前面挂一层 Nginx 做 TLS 终止与反向代理，见 [手动部署](./manual-deploy)。
