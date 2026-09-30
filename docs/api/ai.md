# AI API

AI 辅助阅读（书籍记忆/世界观）与 AI 模型配置、代理接口。JSON 接口响应包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

## 书籍 AI 记忆

记忆数据按「用户 + 书籍」隔离存储；操作目标书籍必须已在当前用户书架中。

### 获取书籍记忆

```text
GET /reader3/getAiBookMemory?bookUrl=<书籍URL>
```

`bookUrl` 别名 `url`。响应 `data` 为 `AiBookMemory` 对象（不存在时为 null）。

### 保存书籍记忆

```text
POST /reader3/saveAiBookMemory
```

请求体为 `AiBookMemory` 对象（`bookUrl` 必填；`bookName`/`author` 为空时自动取书架上的书名/作者）。响应 `data` 为保存后的对象。

### 删除书籍记忆

```text
POST /reader3/deleteAiBookMemory
```

参数同获取。响应 `data` 为 `{ "deleted": true }`。

`AiBookMemory` 主要字段：`bookUrl`、`bookName?`、`author?`、`enabled`、`processedChapterIndex?`、`processedChapterTitle?`、`updatedAt`、`summary`、`worldview[]`、`characters[]`、`relationships[]`、`locations[]`、`map?`、`mapDirty`。

## AI 资料编排任务

AI 资料的生成循环运行在**后端拉起的 Python sidecar 进程**里（批量章节循环在 Rust 侧）。
前端只触发、轮询与展示；任务在服务端跑，**关页面不取消**。三个端点均为 `POST`、需登录。

### 提交任务

```text
POST /reader3/runAgentTask
```

请求体：

| 参数                 | 类型    | 说明                                                                    |
| -------------------- | ------- | ------------------------------------------------------------------------ |
| `bookUrl`            | string  | 书架上的书籍 URL（必填）                                                 |
| `kind`               | string  | `update_to_current`（更新到当前进度，默认）或 `redraw_map`（重绘地图）   |
| `targetChapterIndex` | number? | 目标章 index；缺省取书架阅读进度                                         |

响应 `data` 为 `{ "jobId": string }`。已有任务进行中返回 **HTTP 409**（单用户单任务）。

### 查询任务状态

```text
POST /reader3/getAgentTaskStatus
```

响应 `data`：`{ "running": bool, "jobId": string, "bookUrl": string, "phase": "idle"|"loading"|"text"|"map"|"saving"|"error", "statusText": string, "currentChapterIndex": number|null, "targetChapterIndex": number|null, "lastError": string|null }`。

状态快照在任务结束后保留（含 `lastError`），直到下一个任务提交或服务重启。

### 取消任务

```text
POST /reader3/cancelAgentTask
```

取消 = 终止 sidecar 进程并中止批量；响应 `data` 为 `{ "cancelled": bool }`（无进行中任务时为 false）。

> 任务结果不设独立端点：轮询到完成后前端复用 `getAiBookMemory` 重拉资料。
> sidecar 配置见 [配置 · AI 资料编排](../guide/configuration.md)（`AGENT_SIDECAR_*`）。

## AI 模型配置

### 获取后端模型可用状态

```text
GET /reader3/getAiModelConfig
```

响应 `data`：`{ "canUseServerModel": bool, "textReady": bool, "imageReady": bool, "speechReady": bool, "agentReady": bool }`。

`agentReady` 表示 AI 资料编排 sidecar 是否就绪（启动时握手探测的结果），仅作提示。

> 后端模型配置只通过环境变量维护（`AI_TEXT_*` / `AI_IMAGE_*` / `AI_SPEECH_*`，见
> [配置 · AI 模型](../guide/configuration.md)），**从不下发到浏览器**——接口只返回
> 「是否配置可用」的布尔值，地址/Key/模型名均不可见。原 `POST /reader3/saveAiModelConfig`
> 已移除。

## AI 代理

### 通用代理

```text
POST /reader3/aiProxy
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

> **注意**：`useServerConfig=false`（客户端自带端点，含 `fullUrl=true` 任意路径）等价于向任意地址发 POST 的通用代理——单用户下登录者即所有者，不再收敛；公网暴露时请配合 `ALLOW_PRIVATE_NETWORK=false`。

响应为上游响应体原样透传（上限 32MB，非 JSON 包装）。所有目标地址经过出站守卫（`ALLOW_PRIVATE_NETWORK=false` 时拒绝私网/环回/链路本地地址，含重定向逐跳检查）。

### 图片代理

```text
POST /reader3/aiProxyImage
```

请求体：`{ "url": "https://..." }`。拉取远程图片并回传（上限 20MB），用于绕开图片防盗链。URL 仅允许 http/https 且过出站守卫。

> **未实现调用方**：本端点目前无前端调用方（AI 资料的地图图片改由后端直接下载落盘到
> `ASSETS_DIR`，依据：`src/service/agent_sidecar_service.rs`），端点保留备用。
