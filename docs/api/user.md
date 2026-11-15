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

## 阅读统计（v1）

阅读统计已从 userdata 迁到服务端明细表，前端不再写 `reading-stats` /
`readChapterHistory` 两份文档。天边界按服务器时区固定偏移计算：**本地 04:00
起算新一天**（凌晨 03:59:59 仍属前一天），不处理 DST。

### 上报阅读事件

```text
POST /v1/me/read-events:batch
```

请求体：

```json
{
  "dayKey": 20340,
  "events": [
    { "bookId": "book_1", "chapterIndex": 3, "seconds": 120 }
  ]
}
```

- `events` 最多 **100** 条；空数组合法（用于只取服务端 `dayKey`）；
- `dayKey` 可选：客户端本地累计值所属的服务端 dayKey（上次响应拿到的值）。
  与当前日不同时事件写回该历史日（跨日/离线缓冲），响应始终返回当前 dayKey；
  缺省按当前日写入。离线跨多日时客户端仍只有最后一次成功响应的 dayKey，
  这些秒数会归到该日，不按自然日拆分；
- `seconds` 是该章**当日累计秒数**，服务端截断到 `0..=86400` 并按
  `(用户, 书, 章, 日)` 取 `MAX(旧值, 新值)`——重复上报不会翻倍；
- `chapterIndex` 必须 ≥0；书不在 `books` 登记表 → **整批 404 `BOOK_NOT_FOUND`**
  （不做部分成功）；
- 同一事务里把该书 `shelf_books.dur_chapter_time` 推进到 `MAX(现有, now)`。

响应：

```json
{ "accepted": 1, "dayKey": 20340 }
```

`dayKey` 是服务端当前自然日（整数 epoch day）。客户端用它校准「当日累计秒数」
的清零时机：`dayKey` 变化时把本地累计值整体清零。

### 读取统计

```text
GET /v1/me/stats
```

```json
{
  "totalSeconds": 12345,
  "todaySeconds": 600,
  "readChapters": 87,
  "finishedBooks": 2,
  "streakDays": 5
}
```

- `totalSeconds`：全部明细秒数之和 + 旧版 `reading-stats.totalSeconds` 回填总额；
- `todaySeconds`：今天（服务端 dayKey）的真实阅读秒数，排除回填行；
- `readChapters`：去重后的 `(bookId, chapterIndex)` 数（含回填）；
- `finishedBooks`：书架内「最后一章在明细表里有行」的本数（网络书按章节登记表
  的最大 index，本地书回退书架 `totalChapterNum - 1`）；
- `streakDays`：从今天往前连续、每天真实阅读秒数 >0 的天数；**今天还没读时从
  昨天起算**（只要没整天不读就还在连击中），今天与昨天都没读则为 0；排除回填日。

> 启动时会幂等回填一次旧 userdata：`readChapterHistory` 的已读章节按当前目录
> 反查 index（查不到的丢弃），全部落在回填当天并标 `backfilled=1`（不计今日/
> 连续天数）；`reading-stats.totalSeconds` 记入 `read_stats_legacy`。
> `readChapterHistory` 仍可被旧浏览器读取，但前端新版本不再写入。

## 读取用户数据文档

```text
GET /reader3/getUserdata?name=<文档名>
```

> **废弃**：保留兼容，新客户端请改用 [批量读取](#批量读取用户数据文档)（`GET /reader3/getUserdataBatch`，单文档传单元素 `names` 即可）。

读取当前用户命名空间下的一个 JSON 文档，用于**跨浏览器同步**的轻量用户数据（如最近阅读）。文档不存在时 `data` 为 `null`。阅读统计/已读标记已迁到 `/v1/me/stats` 与 `/v1/me/read-events:batch`，不再写入这两个文档。

- `name` 仅允许字母数字、`.`、`_`、`-`，首字符必须是字母数字，长度 ≤128（文档名历史上曾拼进文件路径，约束保留下来以保持键形态一致）；
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

> **废弃**：保留兼容，新客户端请改用 [批量保存](#批量保存用户数据文档)（`POST /reader3/saveUserdataBatch`，单文档传单元素 `docs` 即可）。

请求体：

```json
{
  "name": "recent-books",
  "value": [{ "bookId": "book_1" }]
}
```

**整文档覆盖写**：`value` 可以是任意 JSON（对象/数组/标量），服务端不做合并——多端合并语义由客户端负责（前端内置的最近阅读会在启动时做并集合并后回写）。响应 `data` 为空字符串；`name` 不合法或缺少 `value` 时返回 `isSuccess: false`。

## 批量读取用户数据文档

```text
GET /reader3/getUserdataBatch?names=<名称1>,<名称2>,...
```

一次读取当前用户的多个 JSON 文档，把首屏的 N 次单文档 GET（如最近阅读）合成一次往返。

- `names` 逗号分隔，**最多 20 个**，逐个校验 `name` 规则（同上），任一非法返回 400；`names` 缺失或为空返回 400；
- 同名条目在响应映射里只出现一次；
- 不存在的文档在 `docs` 映射里对应 `null`。

```json
{
  "isSuccess": true,
  "data": {
    "docs": {
      "recent-books": [{ "bookId": "book_1" }],
      "personal-config": null
    }
  },
  "errorMsg": ""
}
```

## 批量保存用户数据文档

> **v1**：`PUT /v1/me/userdata:batch`，请求体相同；文档级非法→422 + JSON Pointer，单文档超 4MB→413；前端写队列与 pagehide keepalive 已切。批量读 `GET /reader3/getUserdataBatch` 仍留 legacy 且**只读兼容**——阅读统计/已读标记已迁到 v1，不再有写入方。

```text
POST /reader3/saveUserdataBatch
```

请求体：

```json
{
  "docs": [
    { "name": "recent-books", "value": [{ "bookId": "book_1" }] },
    { "name": "personal-config", "value": { "theme": "dark" } }
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
