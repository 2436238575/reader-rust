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

## 请求方法

本项目**只注册了 `GET` 与 `POST`**，没有 `PUT` / `DELETE`（依据：`src/api/router.rs`）。

| 用法           | 说明                                                                                     |
| -------------- | ---------------------------------------------------------------------------------------- |
| `GET`          | 读取类接口，参数走 query string                                                          |
| `POST`         | 写入类接口与「参数较多/含敏感字段」的读取类接口，参数走 JSON body                        |
| `GET` + `POST` | 不少接口同时注册两种方法，同名参数两种传法都接受                                         |
| `any`          | `bookSourceProxy`、`bookSourceClientLog`、`webdav` 系列，以及兜底的 `/reader3(/*rest)`，接受任意 HTTP 方法 |

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
- [章节内容](./chapter) - 获取章节列表和正文、缓存与进度
- [缓存管理](./cache) - 缓存清理与占用统计
- [账号](./user) - 登录、配置与文件

## 认证

鉴权采用 **Access Token + Refresh Token** 双令牌：

| 令牌          | 形态                          | 有效期                         | 用途                                               |
| ------------- | ----------------------------- | ------------------------------ | -------------------------------------------------- |
| Access Token  | HS256 JWT                     | 6 小时（`ACCESS_TOKEN_TTL_SECS`） | 访问业务接口；可放请求头或查询参数                 |
| Refresh Token | `rt_` + 随机串（一次性）      | 7 天滑动续期（`REFRESH_TOKEN_TTL_SECS`） | 只用于 `POST /reader3/refreshToken` 换新，不进 URL |

Access Token 载荷：

| Claim         | 含义                                   |
| ------------- | -------------------------------------- |
| `sub`         | 用户名                                 |
| `uid`         | 自增身份 id；命名空间由它推导为 `user_<uid>` |
| `iat` / `exp` | 签发时间 / 过期时间（默认 6 小时）     |
| `ver`         | 撤销版本号，对应 `users.token_version` |

令牌**只以三种方式传递**（依据：`src/auth/middleware.rs`、`src/api/handlers/user.rs`）：

| 传法     | 位置                          | 用途                                  |
| -------- | ----------------------------- | ------------------------------------- |
| 请求头   | `Authorization: Bearer <jwt>` | 常规请求（仅 Access Token）           |
| 查询参数 | `accessToken=<jwt>`           | SSE 与 `<img>` 等无法设置请求头的场景 |
| JSON body | `{"refreshToken":"rt_..."}`  | 仅 `/reader3/refreshToken` 与登出     |

请求头优先于查询参数。裸令牌（不带 `Bearer ` 前缀）也能识别，但推荐始终带上前缀。

### 刷新与轮换

`POST /reader3/refreshToken` 是公开接口，请求体 `{ "refreshToken": "rt_..." }`，成功返回新的 `accessToken` 与 `refreshToken`。

- **一次性**：换取成功即消费旧令牌，同时下发新令牌；
- **30 秒宽限窗口**：客户端丢响应或双标签页并发时，已消费令牌在窗口内仍可换取；超过窗口再次出现视为重放，吊销整条登录会话；
- **7 天滑动续期**：每次换取重新计时；
- Refresh Token 不是 JWT，不能当访问凭据；Access Token 也不能用于刷新；
- 失败统一 **HTTP 401 + `需要登录`**。

### 需要登录的接口

除 `/health`、`/reader3/login`、`/reader3/refreshToken`、`/reader3/getUserInfo`、`/reader3/logout` 与 WebDAV 的 Basic 认证入口外，**所有 `/reader3/*` 接口都要求有效 Access Token**，否则返回 401。

`/reader3/getUserInfo` 与 `/reader3/logout` 是**可选鉴权**：未登录也返回 200，只是 `userInfo` 为 `null` / 仅做本地清理确认。

### 令牌失效

Access Token 无状态，但服务端每个请求都会比对 `users.token_version`；Refresh Token 另有 `refresh_tokens` 表记录轮换状态。以下操作会让会话立即失效：

- 修改密码（`changePassword`）——自增版本号并吊销该账号全部 Refresh Token，响应为当前设备新开一对令牌
- 以不同的 `ADMIN_PASSWORD` 重启服务（强制重置，见 [账号 API](./user#账号的创建与初始密码)）
- 登出（`logout`）——吊销 body 里那枚 Refresh Token 所属的会话家族；当前 Access Token 最迟 6 小时后自然过期
- 已消费的 Refresh Token 在 30 秒宽限窗口外再次提交——按泄露处理，吊销整条会话家族

[认证方式的完整说明见账号 API](./user)
