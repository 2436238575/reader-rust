# 用户管理 API

用户注册/登录、配置与管理接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

登录后服务端返回一个 **JWT** 作为 `accessToken`，请求时放在 `Authorization` 头，例如 `Authorization: Bearer <jwt>`。令牌载荷与失效规则见 [API 概述 · 认证](./index#认证)。

## 用户登录

```text
POST /reader3/login
```

请求体：

```json
{
  "username": "用户名",
  "password": "密码",
  "isLogin": false,
  "code": "邀请码（可选）"
}
```

- `isLogin` 为 `true` 时仅登录；为 `false` 且用户名不存在时，自动创建该用户（注册）。
- 注册校验规则：用户名至少 5 位、仅小写字母与数字；密码至少 8 位；配置了 `INVITE_CODE` 时 `code` 必填。
- **登录限速**：`isLogin=true` 的登录在 10 分钟窗口内失败满 8 次即锁定该用户名 5 分钟，期间任何登录尝试（含正确密码）返回 400 与「登录失败次数过多，请 N 秒后再试」。WebDAV Basic 认证共享同一份计数。

响应：

```json
{
  "isSuccess": true,
  "data": {
    "accessToken": "<JWT>",
    "username": "用户名",
    "lastLoginAt": 0,
    "enableWebdav": false,
    "enableLocalStore": false,
    "enableAiModel": false,
    "createdAt": 0,
    "isAdmin": false
  },
  "errorMsg": ""
}
```

## 退出登录

```text
POST /reader3/logout
```

**总是返回成功**，且不要求有效令牌。JWT 是无状态的，服务端没有可撤销的会话记录，该接口只是给客户端一个「可以清除本地令牌」的确认；真正使令牌失效的是改密码/重置密码/删号（见下）。

## 获取用户信息

```text
GET /reader3/getUserInfo
```

可选鉴权：未登录也返回 200，此时 `userInfo` 为 `null`。

响应 `data` 形如：

```json
{ "userInfo": { "username": "用户名", "isAdmin": false, "...": "..." }, "adminAuthorized": false }
```

`userInfo` 不含任何凭据字段；`adminAuthorized` 表示当前令牌是否具备管理员角色。

## 修改密码

```text
POST /reader3/changePassword
```

请求体：

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

响应 `data` 为自由 JSON：服务端直接返回 `userConfig.json` 中存储的内容，结构由调用方（前端）约定，服务端不校验固定字段。常见字段如 `theme`、`fontSize`、`lineHeight`、`readConfig` 仅为前端约定，非服务端结构（依据：`src/service/user_service.rs` 的 `get_user_config`）。

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

仅管理员可用，否则 403。响应：`data` 为用户对象数组，**不含任何凭据字段**——JWT 由各设备自行持有，服务端不回吐他人令牌：

```json
{
  "isSuccess": true,
  "data": [
    {
      "username": "用户名",
      "lastLoginAt": 1699999999999,
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

仅管理员可用，否则 403。请求体：

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

仅管理员可用，否则 403。请求体为用户名数组。响应：`data` 为剩余用户列表。

## 重置用户密码

```text
POST /reader3/resetPassword
```

仅管理员可用，否则 403。请求体：

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

仅管理员可用，否则 403。请求参数：

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

查询参数：`force`（boolean，可选，是否强制刷新缓存）。响应 `data` 为 `VersionUpdateInfo`（`src/service/update_service.rs` 的 `VersionUpdateInfo`，camelCase）：

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

仅管理员可用，否则 403。请求体：

```json
{ "version": "v1.0.9" }
```

`version` 为要忽略的版本号；忽略后 `getVersionUpdate` 对该版本不再提示。

## 用户注册

```text
POST /reader3/register
```

> **未实现**：该接口尚未在代码中实现（依据：`src/api/router.rs` 查无 `register` 路由；注册通过 `POST /reader3/login` 且 `isLogin=false` 隐式完成）。

## 上传资源文件

```text
POST /reader3/uploadFile?type=<目录名>
```

`multipart/form-data`，字段名 `file`。文件落到 `ASSETS_DIR/<用户>/<type>/` 并可经 `/assets/...` 静态访问。

约束：`type` 仅允许字母数字、下划线、连字符；文件名只取纯文件名部分，拒绝路径分隔符、Windows 保留字符与设备名（`CON`/`NUL` 等）、结尾点/空格；单文件上限 32MB。响应 `data` 为上传成功的 URL 字符串数组：`["/assets/<用户>/<type>/<文件名>"]`。

## 删除资源文件

```text
POST /reader3/deleteFile
```

请求体：`{ "url": "/assets/<用户>/<type>/<文件名>" }`。只能删除当前用户 `assets` 目录内的文件，路径含 `..` 一律拒绝。响应 `data` 为空字符串。

## 获取 TXT 目录规则

```text
GET /reader3/getTxtTocRules
```

响应 `data` 为规则数组。

> **未实现**：当前恒返回空数组（依据：`src/api/handlers/book.rs` 的 `get_txt_toc_rules`），TXT 导入使用内置默认章节规则。
