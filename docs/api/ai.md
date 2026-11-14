# AI API

AI 辅助阅读（书籍资料/世界观/地图）、模型可用状态、AI 资料编排任务与模型代理。

前端已全部切到 `/v1`：资源接口是具名 schema，错误是 problem+json。
下面先写 `/v1` 契约，随后列出仍可用的 `/reader3` 兼容端点。

## 通用约定

- 所有端点需要登录（Cookie 或 `Authorization: Bearer`）。
- `/v1` 的非 GET 请求必须带 `X-Requested-With: XMLHttpRequest`（Bearer 在场豁免）。
- 书籍资料与任务的身份参数都是路径里的 `bookId`（`book_<n>`）；`bookUrl`
  只是数据字段，不参与「同 id 同书」校验。
- 模型配置本体（地址 / Key / 模型名）只存在于服务端环境变量，任何接口都
  不下发；浏览器经 `/v1/ai/proxy` 间接使用，全程接触不到 Key。

## AI 模型可用状态

```text
GET /v1/me/ai/model-status
```

响应：

```json
{
  "canUseServerModel": true,
  "textReady": true,
  "imageReady": false,
  "speechReady": true,
  "agentReady": true
}
```

`agentReady` 是 AI 资料编排 sidecar 启动握手探测的结果，仅作提示；
`false` 不影响文本/图片/语音模型。

## 书籍 AI 资料

资料按「用户 + `bookId`」隔离存储，不要求书籍仍在书架（移出书架后仍可
读回或清理）。

### 读取

```text
GET /v1/books/{bookId}/ai-memory
```

- 200：`AiBookMemory` 对象。
- 404 `BOOK_NOT_FOUND`：书籍不存在。
- 404 `AI_MEMORY_NOT_FOUND`：书籍存在但尚未生成资料（前端按空骨架渲染）。

`AiBookMemory` 主要字段：`bookId`、`bookUrl`（冗余快照，保存时以登记表当前
绑定为准）、`bookName?`、`author?`、`enabled`、`processedChapterIndex?`、
`processedChapterTitle?`、`updatedAt`、`summary`、`worldview[]`、
`characters[]`、`relationships[]`、`locations[]`、`map?`、`mapDirty`、
`lastError?`。

### 保存（整体替换）

```text
PUT /v1/books/{bookId}/ai-memory
```

请求体是完整的 `AiBookMemory`（`bookId`/`bookUrl` 可省略，服务端以路径与
登记表为准；`bookName`/`author` 为空时自动取登记表）。序列化超过 **1MB**
返回 413 `PAYLOAD_TOO_LARGE`。响应是保存后的对象。

### 删除

```text
DELETE /v1/books/{bookId}/ai-memory
```

幂等，恒 204；书籍不存在时 404。

## AI 资料编排任务

生成循环运行在后端拉起的 Python sidecar 进程里。v1 把 sidecar 的任务快照
接进**统一任务注册表**：提交返回标准 `TaskView`，状态、事件流与取消都走
`/v1/tasks` 的既有机制，客户端不再需要轮询 sidecar 专用接口。

### 提交任务

```text
POST /v1/books/{bookId}/agent-tasks
```

请求体：

| 参数                 | 类型    | 说明                                                   |
| -------------------- | ------- | ------------------------------------------------------ |
| `kind`               | string? | `update-to-current`（默认）或 `redraw-map`             |
| `targetChapterIndex` | number? | 目标章 index（≥0）；缺省取书架阅读进度                 |

响应 **202** + `Location: /v1/tasks/{id}`，body 是 `TaskView`：

- `kind = "ai-generate"`
- `progress.subjectId` 是 `bookId`
- `progress.phase` 是 sidecar 阶段：`loading` / `text` / `map` / `saving` /
  `idle`；失败或取消后任务态是 `failed` / `cancelled`
- `progress.done` / `progress.total` 是「已处理章数 / 目标章数」（1 起）

已有 AI 任务进行中（sidecar 单用户单任务）→ 409 `TASK_ALREADY_RUNNING`。
书籍不在书架 → 409 `BOOK_NOT_IN_SHELF`；书籍不存在 → 404 `BOOK_NOT_FOUND`。
并发任务超限 → 429 `TASK_LIMIT` + `Retry-After`。

### 查询最近一次任务

```text
GET /v1/books/{bookId}/agent-task
```

返回这本书最近一次任务（含已结束；服务重启后任务随内存注册表消失）。
没有任务时 404 `TASK_NOT_FOUND`。页面刷新后用它可以恢复进度显示。

通用任务端点同样适用：

- `GET /v1/tasks/{id}`：任务状态（支持 ETag/304）
- `GET /v1/tasks/{id}/events`：SSE 事件流（`Last-Event-ID` 续传）
- `DELETE /v1/tasks/{id}`：取消任务

### 取消

```text
DELETE /v1/books/{bookId}/agent-task
```

请求取消这本书正在跑的 AI 任务（终止 sidecar 进程），幂等 204；已结束或
没有任务也返回 204。**只取消这本书的任务**：sidecar 全局单任务，若当前
跑的是另一本书则不会被误杀。

> 任务结果不设独立端点：完成后前端用 `GET /v1/books/{bookId}/ai-memory`
> 重拉资料。失败原因在 `TaskView.error.detail`（同时写进资料的 `lastError`）。

## AI 模型代理

### 通用代理

```text
POST /v1/ai/proxy
```

请求体（`AiProxyRequest`）：

| 参数              | 类型    | 说明                                                                                                    |
| ----------------- | ------- | ------------------------------------------------------------------------------------------------------- |
| `useServerConfig` | boolean | true 时使用服务端配置的模型，忽略下面的 baseUrl/apiKey                                                  |
| `baseUrl`         | string  | 客户端自定的上游地址（`useServerConfig=false` 时必填）                                                  |
| `apiKey`          | string? | 客户端自带密钥，仅加入出站请求头，不回传                                                                |
| `path`            | string  | API 路径；`fullUrl=false` 时仅允许 `/v1/chat/completions`、`/v1/images/generations`、`/v1/audio/speech` |
| `fullUrl`         | boolean | true 时 `baseUrl` 即完整请求地址（跳过路径白名单）                                                      |
| `kind`            | string? | `text`/`image`/`speech`，用于服务端配置的默认值补全                                                     |
| `body`            | object  | 透传给上游的请求体                                                                                      |

成功响应**流式透传**（上游边发边转发，`stream:true` 的客户端按块收到数据；
流上累计超过 32MB 即掐断）。错误统一 problem+json：

- 400 `MALFORMED_REQUEST`：URL/路径非法或出站守卫拒绝
- 502 `UPSTREAM_FAILED`：上游非 2xx / 网络失败；`detail` 给可读摘要，
  `upstreamCode` 带真实上游状态码

> **超时**：与上传路由同档使用 `INBOUND_UPLOAD_TIMEOUT_SECS`（默认 900 秒），
> 不受常规接口的 120 秒入站超时约束——非流式调用（不带 `stream:true` 的
> chat、图像、语音生成）的响应头要等上游生成完才返回。上游调用自身另有
> 300 秒客户端预算，超时会返回可读错误而不是 HTTP 408。

> `useServerConfig=false`（客户端自带端点，含 `fullUrl=true` 任意路径）等价于
> 向任意地址发 POST 的通用代理。单用户部署下登录者即所有者；出站安全由
> `ALLOW_PRIVATE_NETWORK`（默认拦截私网/环回/链路本地，含重定向逐跳检查）
> 负责。

### 图片代理

```text
POST /v1/ai/proxy-image
```

请求体：`{ "url": "https://..." }`。拉取远程图片并回传（上限 20MB），用于
绕开图片防盗链。URL 仅允许 http/https 且过出站守卫；**上游响应的
Content-Type 必须是位图格式 `image/jpeg`/`image/png`/`image/gif`/
`image/webp`/`image/avif`/`image/bmp`**，否则 400。`image/svg+xml` 等可以
携带脚本的类型同样会被拒绝，防止可控 URL 返回的 HTML 或 SVG 在本站域名下
执行。

> 本端点目前没有前端调用方（AI 资料的地图图片由后端直接下载落盘到
> `ASSETS_DIR`），保留备用。

## `/reader3` 兼容端点

以下 legacy 端点继续可用（旧客户端/脚本），响应仍是 `ApiResponse` 包裹；
前端已无调用方：

| 端点                                | 说明                                                    |
| ----------------------------------- | ------------------------------------------------------- |
| `GET /reader3/getAiBookMemory`      | 读取书籍资料（不存在时 `data: null`）                   |
| `POST /reader3/saveAiBookMemory`    | 保存资料                                                |
| `POST /reader3/deleteAiBookMemory`  | 删除资料                                                |
| `GET /reader3/getAiModelConfig`     | 模型可用状态（布尔值）                                  |
| `POST /reader3/runAgentTask`        | 提交任务（`kind` 为 `update_to_current` / `redraw_map`） |
| `POST /reader3/getAgentTaskStatus`  | sidecar 快照（全局最近一条）                            |
| `POST /reader3/cancelAgentTask`     | 取消当前任务                                            |
| `POST /reader3/aiProxy`             | 通用代理（错误响应为 `ApiResponse`，上游状态码原样透传） |
| `POST /reader3/aiProxyImage`        | 图片代理                                                |

legacy 任务与 v1 任务共用同一个 sidecar 单任务槽：任一侧提交后另一侧都会
看到「已有任务进行中」。legacy 快照与 v1 任务注册表各自保留一份状态，互不
替代。

sidecar 配置见 [配置 · AI 资料编排](../guide/configuration.md)（`AGENT_SIDECAR_*`）。
