# API 概述

Reader-Rust 的契约接口在 `/v1`（认证、书架、书源、书籍/章节、任务、文件等），`/reader3` 只保留尚未迁移的过渡端点；`GET /health` 不带前缀。

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
http://localhost:8080/v1
```

`/reader3` 是尚未迁移的过渡命名空间；新客户端一律使用 `/v1`。

## 响应格式

### `/v1`

成功响应是业务对象的裸 JSON（不包 `isSuccess`）。失败统一为 `application/problem+json`：

```json
{
  "code": "VALIDATION_FAILED",
  "status": 422,
  "title": "Validation failed",
  "detail": "字段值不合法",
  "errors": [{ "pointer": "/name", "code": "REQUIRED", "detail": "不能为空" }],
  "traceId": "…"
}
```

- `code` 是稳定错误码，客户端用它分支；`detail` 是中文提示，可直接展示。
- `errors[].pointer` 是 JSON Pointer，表单/批量接口按它定位字段。
- `traceId` 与服务端日志的 `X-Request-Id` 一致。

### `/reader3`（过渡）

仍返回 `src/error/error.rs` 的 `ApiResponse<T>`：

```json
{ "isSuccess": true, "errorMsg": "", "data": {} }
```

失败时 `isSuccess=false`，`errorMsg` 是用户可读文案；未登录/令牌失效为 HTTP 401 + `errorMsg="需要登录"`。

### 认证与超时

- `/v1` 未登录或令牌失效 → 401 problem+json（常见 `AUTH_EXPIRED`）；`/reader3` → 401 + `ApiResponse`。
- `/v1` 入站处理超时 → 504 problem+json `PROCESSING_TIMEOUT`；`/reader3` → 408 + `ApiResponse`「请求超时」。
- 豁免：SSE 在开始返回事件流后不受入站超时约束；WebDAV 协议端点（流式传输）；大上传路由与 AI 代理路由使用更长的 `INBOUND_UPLOAD_TIMEOUT_SECS`（默认 900 秒）。
- 需登录接口共享 `INBOUND_MAX_CONCURRENCY`（默认 256）并发预算，排队时间计入超时。

## 请求方法

`/v1` 使用 REST 方法：

| 方法     | 说明                                       |
| -------- | ------------------------------------------ |
| `GET`    | 读取；参数走 query string                  |
| `POST`   | 创建/动作/批量操作                         |
| `PUT`    | 整体替换或幂等 upsert                      |
| `PATCH`  | 局部更新                                   |
| `DELETE` | 删除                                       |

`/v1` 的非 GET 必须带 `X-Requested-With: XMLHttpRequest`（Bearer 客户端豁免）。`/reader3` 过渡端点仍以 GET/POST 为主，`bookSourceProxy` 与 WebDAV 系列接受任意方法。

## API 分类

- [书源管理](./book-source) - 书源的增删改查、测试、登录代理与导入导出
- [书籍搜索](./search) - 搜索书籍、发现页、获取详情与目录
- [章节内容](./chapter) - 获取章节列表和正文、缓存与进度、图片管道（`/v1/images/<id>?w=` 与 `/v1/images:resolve`，支持按宽度缩放与 `Accept` 头的 webp 协商；legacy `/reader3/image`、`/reader3/cover` 过渡期保留）
- [缓存管理](./cache) - 缓存清理与占用统计
- [账号](./user) - 登录、会话与用户数据

## 认证

鉴权采用 **Access Token + Refresh Token** 双令牌：

| 令牌          | 形态                          | 有效期                         | 传递                                                                 |
| ------------- | ----------------------------- | ------------------------------ | -------------------------------------------------------------------- |
| Access Token  | HS256 JWT                     | 6 小时（`ACCESS_TOKEN_TTL_SECS`） | 浏览器走 HttpOnly Cookie `reader_access`；API 客户端可放 `Authorization` 头 |
| Refresh Token | `rt_` + 随机串（一次性）      | 7 天滑动续期（`REFRESH_TOKEN_TTL_SECS`） | 只随 `reader_refresh` HttpOnly Cookie 发给 `/v1/auth/refresh` |

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
reader_access=<JWT>; Path=/; HttpOnly; SameSite=Lax; Max-Age=<access TTL>
reader_refresh=<rt_...>; Path=/v1/auth; HttpOnly; SameSite=Lax; Max-Age=<refresh TTL>
```

- 两枚都是 host-only，JS 读不到，代理页也拿不到；`Secure` 由 `COOKIE_SECURE` 控制（`auto` 默认按 Host 是否回环决定，`true` / `false` 强制）。
- 浏览器请求、SSE（`EventSource`）与 `<img>` 都由浏览器自动携带 Cookie，URL 里不再需要 `accessToken`。
- 普通鉴权只认 `Authorization` 与 `reader_access` Cookie；查询串 `?accessToken=` 只由 `/v1/sources/{sourceId}/proxy` 子路由自行读取，其他路径一律忽略。

### API 客户端

| 传法     | 位置                          | 用途                                  |
| -------- | ----------------------------- | ------------------------------------- |
| 请求头   | `Authorization: Bearer <jwt>` | 常规接口                              |
| 查询参数 | `accessToken=<限定令牌>`      | 仅 `/v1/sources/{sourceId}/proxy` 与 `.../proxy/client-log` |

取令牌顺序：`Authorization` 头 → `reader_access` Cookie。查询串令牌只由 v1 代理子路由自行读取并校验，普通鉴权链不接受；裸令牌（不带 `Bearer ` 前缀）也能识别，但推荐始终带上前缀。

### 刷新与轮换

`POST /v1/auth/refresh` 是公开接口，只认 `reader_refresh` HttpOnly Cookie（body 里的 `refreshToken` 字段不受理）。

成功时 `Set-Cookie` 换发两枚新 Cookie，JSON 返回 `accessToken`、`accessTokenExpiresAt` 与 `username`，`refreshToken` 不再交给 JS。失败返回 401 problem+json（普通失效 `AUTH_EXPIRED`、重放 `AUTH_REFRESH_REPLAYED`），并清除两枚 Cookie。

- **一次性**：换取成功即消费旧令牌，同时下发新令牌；
- **30 秒宽限窗口**：客户端丢响应或双标签页并发时，已消费令牌在窗口内仍可换取；超过窗口再次出现视为重放，吊销整条登录会话；
- **7 天滑动续期**：每次换取重新计时；
- Refresh Token 不是 JWT，不能当访问凭据；Access Token 也不能用于刷新；
- 多设备各自持有独立的会话家族（每次登录新建一条），登出只吊销当前家族。

### 需要登录的接口

除 `/health`、`/v1/auth/login`、`/v1/auth/refresh`、`/v1/auth/logout` 与 WebDAV 的 Basic 认证入口外，**`/v1` 与 `/reader3` 的业务接口都要求有效 Access Token**（浏览器由 Cookie 自动携带），否则返回 401。

旧 `/reader3/getUserInfo` 是可选鉴权（未登录返回 200、`userInfo: null`）；新客户端用 `GET /v1/me`。`/v1/auth/logout` 不要求有效 Access Token，总是 204。

### 令牌失效

Access Token 无状态，但服务端每个请求都会比对 `users.token_version`；Refresh Token 另有 `refresh_tokens` 表记录轮换状态。以下操作会让会话立即失效：

- 修改密码（`POST /v1/auth/password`）——自增版本号并吊销该账号全部 Refresh Token，响应为当前设备换发新的 Cookie
- 以不同的 `ADMIN_PASSWORD` 重启服务（强制重置，见 [账号 API](./user#账号的创建与初始密码)）
- 登出（`POST /v1/auth/logout`）——吊销 Cookie 里那枚 Refresh Token 所属的会话家族并清除两枚 Cookie；当前 Access Token 最迟 6 小时后自然过期
- 已消费的 Refresh Token 在 30 秒宽限窗口外再次提交——按泄露处理，吊销整条会话家族

[认证方式的完整说明见账号 API](./user)
