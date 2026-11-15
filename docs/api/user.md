# 账号 API

单用户账号的登录、会话与用户数据接口。`/v1` 的响应是裸 JSON，失败为 `application/problem+json`（`{code,status,title,detail,errors?,traceId}`）；`/reader3` 的旧 `ApiResponse` 包装只存在于未迁移的过渡端点。

登录成功后服务端用两枚 HttpOnly Cookie 承载会话：**Access Token**（6 小时 HS256 JWT）与 **Refresh Token**（7 天一次性随机串）。浏览器不需要读写令牌，SSE 与图片请求也会自动携带 Cookie；API 客户端仍可用 `Authorization: Bearer <jwt>`。Cookie 属性、载荷与轮换规则见 [API 概述 · 认证](./index#认证)。

## 账号的创建与初始密码

**不提供注册接口。** 唯一账号由服务端启动时创建：

- 用户名取 `ADMIN_USERNAME`（默认 `admin`，仅允许小写字母、数字与下划线）；
- 密码取 `ADMIN_PASSWORD`：非空时**每次启动都强制写入**（覆盖现有密码并使旧令牌失效，与现密码相同则不动），因此它也是忘记密码时的找回通道；
- `ADMIN_PASSWORD` 留空且账号尚不存在时，随机生成 20 位强密码并打印到启动日志。

## 登录

```text
POST /v1/auth/login
```

请求体：

```json
{
  "username": "用户名",
  "password": "密码"
}
```

- 非 GET 请求必须带 `X-Requested-With: XMLHttpRequest`（CSRF 双保险；Bearer 客户端豁免）；
- **登录限速**：用户名维度失败 10 次封禁该用户名登录 6 小时；IP 维度失败 5 次封禁该地址登录 6 小时。封禁期间任何登录尝试（含正确密码）返回错误与重试秒数。WebDAV Basic 认证共享同一份计数。`RATE_LIMIT_DISABLED=true` 时豁免。

响应：

```json
{
  "accessToken": "<JWT>",
  "accessTokenExpiresAt": "2026-11-15T12:00:00Z",
  "username": "用户名"
}
```

响应头同时下发 `reader_access` 与 `reader_refresh` 两枚 Cookie（`HttpOnly; SameSite=Lax`；access `Path=/`，refresh `Path=/v1/auth` 由 `COOKIE_PATH` 推导；`Secure` 由 `COOKIE_SECURE` 决定）。`accessToken` 保留给 API 客户端；浏览器不需要保存它，`refreshToken` 永不进 JSON。

`uid` 是后端分配的自增身份 id；存储命名空间一律用 `user_<uid>`，username 仅登录与展示。`accessTokenExpiresAt` 是 RFC 3339 绝对过期时间，前端据此安排主动续期；`refresh_token_ttl_secs` 决定 `reader_refresh` 的 `Max-Age`。

## 刷新令牌

```text
POST /v1/auth/refresh
```

公开接口。只认 `reader_refresh` HttpOnly Cookie，请求体留空（body 里的 `refreshToken` 字段不受理）。

响应与登录相同：

```json
{
  "accessToken": "<新的 JWT>",
  "accessTokenExpiresAt": "2026-11-15T12:00:00Z",
  "username": "用户名"
}
```

响应头 `Set-Cookie` 换发两枚新 Cookie，Refresh Token 重新计时；JSON 不返回 `refreshToken`。

规则：

- **一次性轮换**：每换取一次就作废当前 Refresh Token，同时下发新的 Refresh Token；
- 已消费令牌在 **30 秒宽限窗口**内重复提交仍可换取（客户端断网重试、双标签页并发），窗口外再次出现视为泄露，吊销整条登录会话；
- 有效期 **7 天滑动续期**：每次换取重新计时，持续使用的设备不会掉线；
- Refresh Token 不是 JWT，不能放进 `Authorization` 头访问业务接口；Access Token 也不能用来刷新（哈希查不到）；
- 失败返回 401 problem+json：普通失效 `AUTH_EXPIRED`，重放 `AUTH_REFRESH_REPLAYED`；并清除两枚 Cookie；
- 多设备各自持有独立的会话家族（每次登录新建一条），登出只吊销当前家族。

## 退出登录

```text
POST /v1/auth/logout
```

不需要请求体。服务端只读 `reader_refresh` Cookie，吊销其所属的登录会话家族并清除两枚 Cookie；成功返回 **204**。

登出不会立即作废已经签发的 Access Token——它最迟在 6 小时后自然过期；需要立刻踢掉所有设备请使用改密码。

## 获取用户信息

```text
GET /v1/me
```

需要登录。响应：

```json
{ "id": "u_1", "username": "用户名", "createdAt": "2026-11-15T00:00:00Z" }
```

`id` 是不透明字符串，客户端不要解析。未登录或令牌失效返回 401 problem+json；旧 `/reader3/getUserInfo` 可选鉴权端点仍保留给过渡客户端，但前端已改用 `/v1/me`。

## 修改密码

```text
POST /v1/auth/password
```

请求体：

```json
{
  "currentPassword": "旧密码",
  "newPassword": "新密码"
}
```

新密码至少 8 位。改密码会自增 `token_version` 并吊销该账号全部 Refresh Token：其他设备上的 Access Token 与 Refresh Token 立即失效。成功返回 **204**，并为当前设备换发两枚新 Cookie；浏览器不需要保存令牌。

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

## 用户数据文档（v1）

阅读统计/已读标记已迁到 `/v1/me/stats` 与 `/v1/me/read-events:batch`；userdata 通用文档现在只剩 `recent-books` 等客户端自管数据。读写都走 v1，legacy `/reader3` 文档接口已删除。

### 批量读取

```text
GET /v1/me/userdata:batch?names=<名称1>,<名称2>,...
```

- `names` 逗号分隔，**最多 20 个**；文档名首字符必须是字母数字，长度 ≤128，只含字母数字、`.`、`_`、`-`；空/超限/非法名 → 422 `VALIDATION_FAILED` + `/names` pointer。
- 缺失文档在 `docs` 映射里为 `null`；同名去重。
- 响应总大小上限 16MB，超过返回 413。

```json
{
  "docs": {
    "recent-books": [{ "bookId": "book_1" }],
    "missing-doc": null
  }
}
```

### 批量保存

```text
PUT /v1/me/userdata:batch
```

请求体：

```json
{
  "docs": [
    { "name": "recent-books", "value": [{ "bookId": "book_1" }] }
  ]
}
```

- `docs` 最多 20 个，不能为空；每个文档整文档覆盖写，单文档序列化后 ≤4MB；
- 全部先校验后写入：任一文档名非法、缺 `value` 或超限 → 422/413，一个都不落库；
- 同名文档按最后一次为准，只写一份；
- 响应 `{"saved": 1}`。

`GET` 与 `PUT` 共用同一路径，浏览器会话 Cookie 与 API 客户端的 Bearer 都可鉴权；非 GET 走 `/v1` 的 `X-Requested-With` CSRF 检查（Bearer 豁免）。
