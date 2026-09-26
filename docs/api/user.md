# 账号 API

单用户账号的登录、配置与文件接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

登录后服务端返回一个 **JWT** 作为 `accessToken`，请求时放在 `Authorization` 头，例如 `Authorization: Bearer <jwt>`。令牌载荷与失效规则见 [API 概述 · 认证](./index#认证)。

## 账号的创建与初始密码

**不提供注册接口。** 唯一账号由服务端启动时创建：

- 用户名取 `ADMIN_USERNAME`（默认 `admin`，仅允许小写字母、数字与下划线）；
- 密码取 `ADMIN_PASSWORD`：非空时**每次启动都强制写入**（覆盖现有密码并使旧令牌失效，与现密码相同则不动），因此它也是忘记密码时的找回通道；
- `ADMIN_PASSWORD` 留空且账号尚不存在时，随机生成 20 位强密码并打印到启动日志。

## 登录

```text
POST /reader3/login
```

请求体：

```json
{
  "username": "用户名",
  "password": "密码"
}
```

- **登录限速**：用户名维度失败 10 次封禁该用户名登录 6 小时；IP 维度失败 5 次封禁该地址登录 6 小时。封禁期间任何登录尝试（含正确密码）返回 400 与「登录失败次数过多，请 N 秒后再试」。WebDAV Basic 认证共享同一份计数。`RATE_LIMIT_DISABLED=true` 时豁免。

响应：

```json
{
  "isSuccess": true,
  "data": {
    "accessToken": "<JWT>",
    "username": "用户名",
    "lastLoginAt": 0,
    "createdAt": 0
  },
  "errorMsg": ""
}
```

## 退出登录

```text
POST /reader3/logout
```

**总是返回成功**，且不要求有效令牌。JWT 是无状态的，服务端没有可撤销的会话记录，该接口只是给客户端一个「可以清除本地令牌」的确认；真正使令牌失效的是改密码（见下）与 `ADMIN_PASSWORD` 强制重置。

## 获取用户信息

```text
GET /reader3/getUserInfo
```

可选鉴权：未登录也返回 200，此时 `userInfo` 为 `null`。

响应 `data` 形如：

```json
{ "userInfo": { "username": "用户名", "lastLoginAt": 0, "createdAt": 0 } }
```

`userInfo` 不含任何凭据字段。

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

新密码至少 8 位。改密码会自增 `token_version`：其他设备上的令牌立即失效，响应里携带为当前设备换发的新 `accessToken`（客户端必须保存，否则当前设备也会被登出）。

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

请求体：

```json
{ "version": "v1.0.9" }
```

`version` 为要忽略的版本号；忽略后 `getVersionUpdate` 对该版本不再提示。

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
