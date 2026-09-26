# API 概述

Reader-Rust 提供 RESTful API，所有接口都以 `/reader3` 为前缀（仅 `GET /health` 例外）。

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
  "data": { }
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

| 字段 | 成功时 | 失败时 |
|------|--------|--------|
| `isSuccess` | `true` | `false` |
| `errorMsg` | `""`（**空字符串，不是 `null`**） | 错误描述 |
| `data` | 业务数据 | 通常为 `null`，少数接口会带内容（见下） |

### 两个特殊约定

**`NEED_LOGIN` —— 需要登录**

未登录、令牌无效或令牌已过期，一律返回 **HTTP 401**，响应体为：

```json
{
  "isSuccess": false,
  "errorMsg": "NEED_LOGIN",
  "data": null
}
```

前端对 `errorMsg === "NEED_LOGIN"` 与 HTTP 401 两种信号都会弹出登录框。

（唯一的例外是 WebDAV 的 `/reader3/webdav/*path`，它走 HTTP Basic 认证，返回裸 401，不带上述 JSON 结构。）

## 请求方法

本项目**只注册了 `GET` 与 `POST`**，没有 `PUT` / `DELETE`（依据：`src/api/router.rs`）。

| 用法 | 说明 |
|------|------|
| `GET` | 读取类接口，参数走 query string |
| `POST` | 写入类接口与「参数较多/含敏感字段」的读取类接口，参数走 JSON body |
| `GET` + `POST` | 不少接口同时注册两种方法，同名参数两种传法都接受 |
| `any` | 仅 `bookSourceProxy`、`bookSourceClientLog`、`webdav/*path` 三个路由，接受任意 HTTP 方法 |

因为大量接口同时支持 `GET` 与 `POST`，各接口页面会以 `GET /reader3/xxx` 或 `POST /reader3/xxx` 的形式标注实际注册的方法。

## 路由与 404

`/reader3` 是**纯 API 命名空间**：未注册的路径返回 JSON 格式的 404，而不是落到静态文件服务上。

前端使用 hash 路由，深链接不依赖服务端回落，因此后端**不提供 SPA fallback**：

| 路径 | 行为 |
|------|------|
| `/` | 返回 `WEB_ROOT/index.html` |
| `/assets/*` | 先查 `WEB_ROOT/assets`，再回落到 `ASSETS_DIR` |
| `/sw.js`、`/site.webmanifest`、favicon 等 | `WEB_ROOT` 下的真实文件按文件名直接可取 |
| `/reader3/*` 未注册 | JSON 404 |
| 其他不存在的路径 | 404，**不会**回落 index.html |

## API 分类

- [书源管理](./book-source) - 书源的增删改查、调试与导入导出
- [书籍搜索](./search) - 搜索书籍、发现页、获取详情与目录
- [章节内容](./chapter) - 获取章节列表和正文、缓存与进度
- [缓存管理](./cache) - 缓存清理与占用统计
- [账号](./user) - 登录、配置与文件

## 认证

鉴权基于 **JWT**。登录成功后服务端签发一个标准 JWT（HS256），载荷包含：

| Claim | 含义 |
|-------|------|
| `sub` | 用户名 |
| `ns` | 用户命名空间（数据隔离前缀，当前等于用户名） |
| `iat` / `exp` | 签发时间 / 过期时间（默认 7 天） |
| `ver` | 撤销版本号，对应 `users.token_version` |

令牌**只以两种方式传递**（依据：`src/auth/middleware.rs`）：

| 传法 | 位置 | 用途 |
|------|------|------|
| 请求头 | `Authorization: Bearer <jwt>` | 常规请求 |
| 查询参数 | `accessToken=<jwt>` | SSE 与 `<img>` 等无法设置请求头的场景 |

请求头优先于查询参数。裸令牌（不带 `Bearer ` 前缀）也能识别，但推荐始终带上前缀。

### 需要登录的接口

除 `/health`、`/reader3/login`、`/reader3/getUserInfo`、`/reader3/logout` 与 WebDAV 的 Basic 认证入口外，**所有 `/reader3/*` 接口都要求有效令牌**，否则返回 401。

`/reader3/getUserInfo` 与 `/reader3/logout` 是**可选鉴权**：未登录也返回 200，只是 `userInfo` 为 `null` / 不做任何事。

### 令牌失效

JWT 是无状态的，但服务端每个请求都会比对 `users.token_version`。以下操作会自增该版本号，使该账号**此前签发的所有令牌立即失效**：

- 修改密码（`changePassword`）——响应会换发一个新令牌给当前设备
- 以不同的 `ADMIN_PASSWORD` 重启服务（强制重置，见 [账号 API](./user#账号的创建与初始密码)）

登出（`logout`）不涉及服务端状态：客户端丢弃本地令牌即可，该接口因此总是返回成功。

[认证方式的完整说明见用户管理 API](./user)
