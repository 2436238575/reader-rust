# API 概述

Reader-Rust 提供 RESTful API，所有接口都以 `/reader3` 为前缀（仅 `GET /health` 例外）。

## 健康检查

`GET /health` 无需登录，`version` 为编译期写入的二进制版本号（随发布递增）：

```json
{
  "isSuccess": true,
  "errorMsg": "",
  "data": {
    "status": "ok",
    "version": "0.1.0"
  }
}
```

判断服务是否存活，只需看 HTTP 200 与 `status`；`version` 用来确认运行实例的实际版本，Docker 镜像的 HEALTHCHECK 依赖这个端点。

## 基础 URL

```text
http://localhost:8080/reader3
```

## 响应格式

所有接口统一返回下面的结构（`src/error/error.rs` 的 `ApiResponse<T>`）：

```json
{
  "isSuccess": true,
  "errorMsg": "",
  "data": {}
}
```

失败时：

```json
{
  "isSuccess": false,
  "errorMsg": "错误信息",
  "data": null
}
```

需要注意字段的**实际取值**，不要按直觉推断：

| 字段        | 成功时                            | 失败时                                  |
| ----------- | --------------------------------- | --------------------------------------- |
| `isSuccess` | `true`                            | `false`                                 |
| `errorMsg`  | `""`（**空字符串，不是 `null`**） | 错误描述                                |
| `data`      | 业务数据                          | 通常为 `null`，少数接口会带内容（见下） |

### 两个特殊约定

**需要登录 —— HTTP 401**

未登录、令牌无效或令牌已过期，一律返回 **HTTP 401**，响应体为：

```json
{
  "isSuccess": false,
  "errorMsg": "需要登录",
  "data": null
}
```

`errorMsg` 是用户可读的提示文案（前端会原样 toast 展示）；前端凭 **HTTP 401** 状态码弹出登录框，不识别具体文案。

（唯一的例外是 WebDAV 的 `/reader3/webdav/*path`，它走 HTTP Basic 认证，返回裸 401，不带上述 JSON 结构。）

**请求处理超时 —— HTTP 408**

接口在 `INBOUND_REQUEST_TIMEOUT_SECS`（默认 120 秒）内没有处理完成时，返回 **HTTP 408**，响应体为：

```json
{
  "isSuccess": false,
  "errorMsg": "请求超时",
  "data": null
}
```

覆盖公开、可选登录与需登录三组 `/reader3` 接口（含 `/health`）。豁免：SSE 端点在开始返回事件流后即不受此约束；WebDAV 协议端点（流式传输）；大上传路由（TXT/EPUB 导入、资源上传、`saveBookSources` 等）与 AI 代理路由（`aiProxy`/`aiProxyImage`，非流式生成要等上游出结果）单独使用 `INBOUND_UPLOAD_TIMEOUT_SECS`（默认 900 秒）。需登录接口（含 AI 代理）另有 `INBOUND_MAX_CONCURRENCY`（默认 256）并发上限——全部需登录路由共享同一份并发许可，超出的请求排队等待且排队时间计入超时。

## 请求方法

本项目**只注册了 `GET` 与 `POST`**，没有 `PUT` / `DELETE`（依据：`src/api/router.rs`）。

| 用法           | 说明                                                                                     |
| -------------- | ---------------------------------------------------------------------------------------- |
| `GET`          | 读取类接口，参数走 query string                                                          |
| `POST`         | 写入类接口与「参数较多/含敏感字段」的读取类接口，参数走 JSON body                        |
| `GET` + `POST` | 不少接口同时注册两种方法，同名参数两种传法都接受                                         |
| `any`          | `bookSourceProxy`、`bookSourceClientLog`、`webdav` 系列，以及作为后备的 `/reader3(/*rest)`，接受任意 HTTP 方法 |

因为大量接口同时支持 `GET` 与 `POST`，各接口页面会以 `GET /reader3/xxx` 或 `POST /reader3/xxx` 的形式标注实际注册的方法。

## 路由与 404

`/reader3` 是**纯 API 命名空间**：未注册的路径返回 JSON 格式的 404，而不是落到静态文件服务上。

前端使用 hash 路由，深链接不依赖服务端回落，因此后端**不提供 SPA fallback**：

| 路径                                      | 行为                                          |
| ----------------------------------------- | --------------------------------------------- |
| `/`                                       | 返回 `WEB_ROOT/index.html`                    |
| `/assets/*`                               | 先查 `WEB_ROOT/assets`，再回落到 `ASSETS_DIR` |
| `/sw.js`、`/site.webmanifest`、favicon 等 | `WEB_ROOT` 下的真实文件按文件名直接可取       |
| `/reader3/*` 未注册                       | JSON 404                                      |
| 其他不存在的路径                          | 404，**不会**回落 index.html                  |

## API 分类

- [书源管理](./book-source) - 书源的增删改查、调试与导入导出
- [书籍搜索](./search) - 搜索书籍、发现页、获取详情与目录
- [章节内容](./chapter) - 获取章节列表和正文、缓存与进度、图片管道（`/reader3/image/<id>?w=` 与 `/reader3/cover`，支持按宽度缩放与 `Accept` 头的 webp 协商）
- [缓存管理](./cache) - 缓存清理与占用统计
- [账号](./user) - 登录、配置与文件

## 认证

鉴权采用 **Access Token + Refresh Token** 双令牌：

| 令牌          | 形态                          | 有效期                         | 传递                                                                 |
| ------------- | ----------------------------- | ------------------------------ | -------------------------------------------------------------------- |
| Access Token  | HS256 JWT                     | 6 小时（`ACCESS_TOKEN_TTL_SECS`） | 浏览器走 HttpOnly Cookie `reader_access`；API 客户端可放 `Authorization` 头 |
| Refresh Token | `rt_` + 随机串（一次性）      | 7 天滑动续期（`REFRESH_TOKEN_TTL_SECS`） | 只随 `reader_refresh` HttpOnly Cookie 发给 `/reader3/refreshToken` |

Access Token 载荷：

| Claim         | 含义                                   |
| ------------- | -------------------------------------- |
| `sub`         | 用户名                                 |
| `uid`         | 自增身份 id；命名空间由它推导为 `user_<uid>` |
| `iat` / `exp` | 签发时间 / 过期时间（默认 6 小时）     |
| `ver`         | 撤销版本号，对应 `users.token_version` |

### 浏览器会话

登录、刷新、改密成功时，服务端下发两枚 Cookie：

```text
reader_access=<JWT>; reader_refresh=<rt_...>
HttpOnly; SameSite=Strict; Path=/reader3; Max-Age=<对应 TTL>
```

- 两枚都是 host-only，JS 读不到，代理页也拿不到；`Secure` 由 `COOKIE_SECURE` 控制（`auto` 默认按 Host 是否回环决定，`true` / `false` 强制）。
- 浏览器请求、SSE（`EventSource`）与 `<img>` 都由浏览器自动携带 Cookie，URL 里不再需要 `accessToken`。
- 旧版 localStorage 令牌由前端一次性迁移：启动时用旧 Refresh Token 调用一次刷新，成功后删除旧键；迁移失败也删除旧键并回到登录页。
- 只有 `Authorization`、Cookie 与两个代理路径的查询参数会参与鉴权；其他路径的 `?accessToken=` 一律忽略。

### API 客户端

| 传法     | 位置                          | 用途                                  |
| -------- | ----------------------------- | ------------------------------------- |
| 请求头   | `Authorization: Bearer <jwt>` | 常规接口                              |
| 查询参数 | `accessToken=<限定令牌>`      | 仅 `/reader3/bookSourceProxy` 与 `/reader3/bookSourceClientLog` |

取令牌顺序：`Authorization` 头 → `reader_access` Cookie → 查询参数（仅上面两个代理路径）。裸令牌（不带 `Bearer ` 前缀）也能识别，但推荐始终带上前缀。

### 刷新与轮换

`POST /reader3/refreshToken` 是公开接口，只认 `reader_refresh` HttpOnly Cookie（body 里的 `refreshToken` 字段不受理）。

成功时 `Set-Cookie` 换发两枚新 Cookie，JSON 只返回 `accessToken` 与 `accessTokenExpiresIn`，`refreshToken` 不再交给 JS。失败统一 **HTTP 401 + `需要登录`**，并清除两枚 Cookie。

- **一次性**：换取成功即消费旧令牌，同时下发新令牌；
- **30 秒宽限窗口**：客户端丢响应或双标签页并发时，已消费令牌在窗口内仍可换取；超过窗口再次出现视为重放，吊销整条登录会话；
- **7 天滑动续期**：每次换取重新计时；
- Refresh Token 不是 JWT，不能当访问凭据；Access Token 也不能用于刷新；
- 多设备各自持有独立的会话家族（每次登录新建一条），登出只吊销当前家族。

### 需要登录的接口

除 `/health`、`/reader3/login`、`/reader3/refreshToken`、`/reader3/getUserInfo`、`/reader3/logout` 与 WebDAV 的 Basic 认证入口外，**所有 `/reader3/*` 接口都要求有效 Access Token**（浏览器由 Cookie 自动携带），否则返回 401。

`/reader3/getUserInfo` 与 `/reader3/logout` 是**可选鉴权**：未登录也返回 200，只是 `userInfo` 为 `null` / 仅做本地清理确认。

### 令牌失效

Access Token 无状态，但服务端每个请求都会比对 `users.token_version`；Refresh Token 另有 `refresh_tokens` 表记录轮换状态。以下操作会让会话立即失效：

- 修改密码（`changePassword`）——自增版本号并吊销该账号全部 Refresh Token，响应为当前设备换发新的 Cookie
- 以不同的 `ADMIN_PASSWORD` 重启服务（强制重置，见 [账号 API](./user#账号的创建与初始密码)）
- 登出（`logout`）——吊销 Cookie 里那枚 Refresh Token 所属的会话家族并清除两枚 Cookie；当前 Access Token 最迟 6 小时后自然过期
- 已消费的 Refresh Token 在 30 秒宽限窗口外再次提交——按泄露处理，吊销整条会话家族

[认证方式的完整说明见账号 API](./user)
