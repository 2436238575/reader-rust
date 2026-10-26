# 账号 API

单用户账号的登录、配置与文件接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

登录成功后服务端用两枚 HttpOnly Cookie 承载会话：**Access Token**（6 小时 HS256 JWT）与 **Refresh Token**（7 天一次性随机串）。浏览器不需要读写令牌，SSE 与图片请求也会自动携带 Cookie；API 客户端仍可用 `Authorization: Bearer <jwt>`。Cookie 属性、载荷与轮换规则见 [API 概述 · 认证](./index#认证)。

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
    "accessTokenExpiresIn": 21600,
    "uid": 1,
    "username": "用户名",
    "lastLoginAt": 0,
    "createdAt": 0
  },
  "errorMsg": ""
}
```

响应头同时下发 `reader_access` 与 `reader_refresh` 两枚 Cookie（`HttpOnly; SameSite=Strict; Path=/reader3`，`Secure` 由 `COOKIE_SECURE` 决定）。`accessToken` 保留给 API 客户端；浏览器不需要保存它，`refreshToken` 不再出现在 JSON 里。

`uid` 是后端分配的自增身份 id；存储命名空间一律用 `user_<uid>`，username 仅登录与展示。`accessTokenExpiresIn` 是 Access Token 的有效秒数，前端据此安排主动续期；`refresh_token_ttl_secs` 决定 `reader_refresh` 的 `Max-Age`。

## 刷新令牌

```text
POST /reader3/refreshToken
```

公开接口。只认 `reader_refresh` HttpOnly Cookie，请求体留空（body 里的 `refreshToken` 字段不受理）。

响应 `data`：

```json
{
  "accessToken": "<新的 JWT>",
  "accessTokenExpiresIn": 21600
}
```

响应头 `Set-Cookie` 换发两枚新 Cookie，Refresh Token 重新计时；JSON 不再返回 `refreshToken`。

规则：

- **一次性轮换**：每换取一次就作废当前 Refresh Token，同时下发新的 Refresh Token；
- 已消费令牌在 **30 秒宽限窗口**内重复提交仍可换取（客户端断网重试、双标签页并发），窗口外再次出现视为泄露，吊销整条登录会话；
- 有效期 **7 天滑动续期**：每次换取重新计时，持续使用的设备不会掉线；
- Refresh Token 不是 JWT，不能放进 `Authorization` 头访问业务接口；Access Token 也不能用来刷新（哈希查不到）；
- 任何失败（不存在 / 已过期 / 已吊销 / 重放）统一返回 **HTTP 401 + `需要登录`**，并清除两枚 Cookie；客户端清理本地登录提示后重新登录；
- 多设备各自持有独立的会话家族（每次登录新建一条），登出只吊销当前家族。

## 退出登录

```text
POST /reader3/logout
```

不需要请求体。服务端只读 `reader_refresh` Cookie，吊销其所属的登录会话家族并清除两枚 Cookie。

**总是返回成功**，不要求有效 Access Token，Cookie 缺失或令牌已损坏也不报错。登出不会立即作废已经签发的 Access Token——它最迟在 6 小时后自然过期；需要立刻踢掉所有设备请使用改密码。

## 获取用户信息

```text
GET /reader3/getUserInfo
```

可选鉴权：未登录也返回 200，此时 `userInfo` 为 `null`。

响应 `data` 形如：

```json
{ "userInfo": { "uid": 1, "username": "用户名", "lastLoginAt": 0, "createdAt": 0 } }
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

新密码至少 8 位。改密码会自增 `token_version` 并吊销该账号全部 Refresh Token：其他设备上的 Access Token 与 Refresh Token 立即失效。响应头为当前设备换发两枚新 Cookie，JSON 仍返回 `accessToken` / `accessTokenExpiresIn` 供 API 客户端使用；浏览器不需要保存令牌。

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

## 上传资源文件

```text
POST /reader3/uploadFile?type=<目录名>
```

`multipart/form-data`，字段名 `file`。文件落到 `ASSETS_DIR/<用户>/<type>/` 并可经 `/assets/...` 静态访问。

约束：`type` 仅允许字母数字、下划线、连字符；文件名只取纯文件名部分，拒绝路径分隔符、Windows 保留字符与设备名（`CON`/`NUL` 等）、结尾点/空格；**扩展名只允许位图格式 `jpg`/`jpeg`/`png`/`gif`/`webp`/`avif`/`bmp`（大小写不敏感）；其他扩展名或没有扩展名时返回 400**。`/assets` 与主站同源，上传的内容会直接从本站域名提供；允许 HTML/SVG 等于让上传的文件能在本站域名下执行脚本。单文件上限 32MB；单请求 multipart 字段数上限 20 个（超限 400）。响应 `data` 为上传成功的 URL 字符串数组：`["/assets/<用户>/<type>/<文件名>"]`。

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

## 读取用户数据文档

```text
GET /reader3/getUserdata?name=<文档名>
```

读取当前用户命名空间下的一个 JSON 文档，用于**跨浏览器同步**的轻量用户数据（最近阅读、阅读统计等）。文档不存在时 `data` 为 `null`。

- `name` 仅允许字母数字、`.`、`_`、`-`，首字符必须是字母数字，长度 ≤128（需同时满足后端存储键与旧版文件路径回退的安全约束）；
- 存储在 `json_documents` 表（按 `namespace + name` 唯一），与阅读进度等共用一套用户命名空间。

```json
{
  "isSuccess": true,
  "data": { "totalSeconds": 3600 },
  "errorMsg": ""
}
```

## 保存用户数据文档

```text
POST /reader3/saveUserdata
```

请求体：

```json
{
  "name": "reading-stats",
  "value": { "totalSeconds": 3600 }
}
```

**整文档覆盖写**：`value` 可以是任意 JSON（对象/数组/标量），服务端不做合并——多端合并语义由客户端负责（前端内置的最近阅读与阅读统计会在启动时做并集/取大合并后回写）。响应 `data` 为空字符串；`name` 不合法或缺少 `value` 时返回 `isSuccess: false`。

## 批量读取用户数据文档

```text
GET /reader3/getUserdataBatch?names=<名称1>,<名称2>,...
```

一次读取当前用户的多个 JSON 文档，把首屏的 N 次单文档 GET（最近阅读、阅读统计、已读标记等）合成一次往返。

- `names` 逗号分隔，**最多 20 个**，逐个校验 `name` 规则（同上），任一非法返回 400；`names` 缺失或为空返回 400；
- 同名条目在响应映射里只出现一次；
- 不存在的文档在 `docs` 映射里对应 `null`。

```json
{
  "isSuccess": true,
  "data": {
    "docs": {
      "reading-stats": { "totalSeconds": 3600 },
      "recent-books": null
    }
  },
  "errorMsg": ""
}
```

## 批量保存用户数据文档

```text
POST /reader3/saveUserdataBatch
```

请求体：

```json
{
  "docs": [
    { "name": "reading-stats", "value": { "totalSeconds": 3600 } },
    { "name": "recent-books", "value": [] }
  ]
}
```

- `docs` **最多 20 个**，为空返回 400；每个文档各自整文档覆盖写，单文档沿用 4MB 上限；
- **全部先校验后写入**：任一文档名非法、缺少 `value` 或超限时整批返回 400，一个都不落库；服务层写入再套一层事务，任一条失败整批回滚；
- 同名文档**按最后一次为准**，只写一份；
- 响应 `data.saved` 为实际写入的文档数（去重后）。

```json
{ "isSuccess": true, "data": { "saved": 2 }, "errorMsg": "" }
```
