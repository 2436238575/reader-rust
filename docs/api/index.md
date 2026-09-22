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

通过 `AppError::BadRequest` 返回，因此 HTTP 状态码是 **400**（不是 401），响应体为：

```json
{
  "isSuccess": false,
  "errorMsg": "NEED_LOGIN",
  "data": null
}
```

前端对 `errorMsg === "NEED_LOGIN"`、`data === "NEED_LOGIN"` 以及 HTTP 401 三种情况都会弹出登录框。

（后端唯一的 HTTP 401 来自 WebDAV 处理器 `src/api/handlers/webdav.rs`，走 HTTP Basic 认证，不返回上述 JSON 结构。）

**`NEED_SECURE_KEY` —— 需要管理密码**

这里标记放在 **`data`** 里，不是 `errorMsg`：

```json
{ "isSuccess": false, "errorMsg": "请输入管理密码", "data": "NEED_SECURE_KEY" }
```

出现在需要管理员身份的接口上：保存后端 AI 模型配置、设置默认书源、用户管理、版本更新管理等。

## 请求方法

本项目**只注册了 `GET` 与 `POST`**，没有 `PUT` / `DELETE`（依据：`src/api/router.rs`）。

| 用法 | 说明 |
|------|------|
| `GET` | 读取类接口，参数走 query string |
| `POST` | 写入类接口与「参数较多/含敏感字段」的读取类接口，参数走 JSON body |
| `GET` + `POST` | 不少接口同时注册两种方法，同名参数两种传法都接受 |
| `any` | 仅 `bookSourceProxy`、`bookSourceClientLog`、`webdav/*path` 三个路由，接受任意 HTTP 方法 |

因为大量接口同时支持 `GET` 与 `POST`，各接口页面会以 `GET /reader3/xxx` 或 `POST /reader3/xxx` 的形式标注实际注册的方法。

## API 分类

- [书源管理](./book-source) - 书源的增删改查、调试与导入导出
- [书籍搜索](./search) - 搜索书籍、发现页、获取详情与目录
- [章节内容](./chapter) - 获取章节列表和正文、缓存与进度
- [用户管理](./user) - 用户注册/登录、配置与管理
- [RSS订阅](./rss) - RSS 源管理

## 认证

请求中的身份信息共三个独立字段，每个都支持「请求头」与「URL 查询参数」两种传法（依据：`src/api/auth.rs` 的 `AuthContext`）：

| 字段 | 请求头 | 查询参数 | 用途 |
|------|--------|---------|------|
| 登录凭证 | `Authorization` | `accessToken` | 用户身份 |
| 安全密钥 | `X-Secure-Key` | `secureKey` | 安全模式（管理密码） |
| 数据命名空间 | `X-User-NS` | `userNS` | 多用户数据隔离 |

### 登录凭证

除登录、健康检查等少数接口外，请求需要携带登录返回的 `accessToken`：

```text
Authorization: 用户名:token字符串
```

认证**不是 JWT** —— 服务端自行生成不透明 token，存储于 SQLite（`users.token` 与 `user_sessions` 表），支持多设备同时登录，且服务端可逐会话失效。

该头也接受 `Bearer ` 前缀（大小写均可）：`Authorization: Bearer 用户名:token字符串`。这只是兼容写法，token 本身没有 payload、不能自行解析。

### 安全密钥

开启 `SECURE=true` 后，还需额外校验以下二者之一：

- 请求头 `X-Secure-Key: <管理密码>`
- 查询参数 `secureKey=<管理密码>`

安全密钥与用户登录是**两层独立机制**，可以叠加使用。它同时也用于判定管理员身份 —— 部分接口（保存后端 AI 模型配置、设置默认书源、用户管理、版本更新管理）要求管理员权限，否则返回 `NEED_SECURE_KEY`。

[认证方式的完整说明见用户管理 API](./user)
