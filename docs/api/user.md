# 用户管理 API

用户注册/登录、配置与管理接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

登录后服务端返回不透明 `accessToken`（形如 `用户名:token`），请求时放在 `Authorization` 头，例如 `Authorization: 用户名:token字符串`。认证不是 JWT。

## 用户登录

```text
POST /reader3/login
```

请求体：

```json
{
  "username": "用户名",
  "password": "密码",
  "isLogin": false
}
```

- `isLogin` 为 `true` 时仅登录；为 `false` 且用户名不存在时，自动创建该用户（注册）。

响应：

```json
{
  "isSuccess": true,
  "data": {
    "accessToken": "用户名:token字符串",
    "username": "用户名"
  },
  "errorMsg": ""
}
```

## 退出登录

```text
POST /reader3/logout
```

仅安全模式可用。使当前 token 失效，响应 `errorMsg` 为 `"NEED_LOGIN"`。

## 获取用户信息

```text
GET /reader3/getUserInfo
```

请求头：`Authorization: 用户名:token字符串`。响应 `data` 形如 `{ "userInfo": {...}, "secure": false, "secureKeyRequired": false, "adminAuthorized": false }`。

## 修改密码

```text
POST /reader3/changePassword
```

仅安全模式可用。请求体：

```json
{
  "oldPassword": "旧密码",
  "newPassword": "新密码"
}
```

## 获取用户配置

```text
GET /reader3/getUserConfig
```

响应 `data` 为自由 JSON：服务端直接返回 `userConfig.json` 中存储的内容，结构由调用方（前端）约定，服务端不校验固定字段。常见字段如 `theme`、`fontSize`、`lineHeight`、`readConfig` 仅为前端约定，非服务端结构（依据：`src/service/user_service.rs:183`）。

## 保存用户配置

```text
POST /reader3/saveUserConfig
```

请求体为自由 JSON，原样持久化到 `userConfig.json`。例如：

```json
{
  "theme": "dark",
  "fontSize": 18,
  "lineHeight": 1.8
}
```

## 获取用户列表

```text
GET /reader3/getUserList
```

仅管理员可用（安全模式 + 管理密码）。响应：`data` 为用户对象数组，每个对象为脱敏后的字段（不含 `password` / `salt` / `token`，也不含 `userNamespace`，依据：`src/service/user_service.rs:511` `format_user`）：

```json
{
  "isSuccess": true,
  "data": [
    {
      "username": "用户名",
      "lastLoginAt": 1699999999999,
      "accessToken": "用户名:token",
      "enableWebdav": false,
      "enableLocalStore": false,
      "enableAiModel": false,
      "createdAt": 1699999999999,
      "isAdmin": false
    }
  ],
  "errorMsg": ""
}
```

## 新增用户

```text
POST /reader3/addUser
```

仅管理员可用。请求体：

```json
{
  "username": "用户名",
  "password": "密码"
}
```

## 删除用户

```text
POST /reader3/deleteUsers
```

仅管理员可用。请求体为用户名数组。响应：`data` 为剩余用户列表。

## 重置用户密码

```text
POST /reader3/resetPassword
```

仅管理员可用。请求体：

```json
{
  "username": "用户名",
  "password": "新密码"
}
```

## 更新用户

```text
POST /reader3/updateUser
```

仅管理员可用。请求参数：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `username` | string | 是 | 目标用户名 |
| `enableWebdav` | boolean | 否 | 是否启用 WebDAV |
| `enableLocalStore` | boolean | 否 | 是否启用本地存储 |
| `enableAiModel` | boolean | 否 | 是否启用 AI 模型 |

## 检查版本更新

```text
GET /reader3/getVersionUpdate
```

查询参数：`force`（boolean，可选，是否强制刷新缓存）。仅安全模式需管理员密码。响应 `data` 为 `VersionUpdateInfo`（`src/service/update_service.rs:40`，camelCase）：

| 字段 | 类型 | 说明 |
|------|------|------|
| `currentVersion` | string | 当前版本 |
| `latestVersion` | string? | 最新版本号 |
| `latestName` | string? | 最新版本发布名 |
| `releaseUrl` | string? | 发布页地址 |
| `publishedAt` | string? | 发布时间 |
| `updateAvailable` | boolean | 是否有可用更新 |
| `shouldRemind` | boolean | 是否应提醒 |
| `dismissedVersion` | string? | 已忽略的版本 |
| `checkedAt` | number | 检查时间戳 |
| `error` | string? | 错误信息 |

## 忽略版本更新

```text
POST /reader3/dismissVersionUpdate
```

## 用户注册

```text
POST /reader3/register
```

> **未实现**：该接口尚未在代码中实现（依据：`src/api/router.rs` 查无 `register` 路由；注册通过 `POST /reader3/login` 且 `isLogin=false` 隐式完成）。
