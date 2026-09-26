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

## AI 模型配置

### 获取模型配置

```text
GET /reader3/getAiModelConfig
```

响应 `data`：`{ "config": {...}, "canUseServerModel": bool, "isAdmin": bool }`（后两者为兼容字段，单用户下恒为 `true`）。

### 保存模型配置

```text
POST /reader3/saveAiModelConfig
```

请求体为完整 `AiModelConfig`：

```json
{
  "text":   { "enabled": true, "baseUrl": "https://api.openai.com", "apiKey": "sk-...", "model": "gpt-4o-mini", "useFullUrl": false },
  "image":  { "enabled": false, "baseUrl": "", "apiKey": "", "model": "gpt-image-1", "useFullUrl": false, "imageSize": "1024x1024" },
  "speech": { "enabled": false, "baseUrl": "", "apiKey": "", "model": "gpt-4o-mini-tts", "useFullUrl": false, "voice": "alloy", "responseFormat": "mp3" }
}
```

## AI 代理

### 通用代理

```text
POST /reader3/aiProxy
```

请求体（`AiProxyRequest`）：

| 参数 | 类型 | 说明 |
|------|------|------|
| `useServerConfig` | boolean | true 时使用服务端配置的模型，忽略下面的 baseUrl/apiKey |
| `baseUrl` | string | 客户端自定的上游地址（`useServerConfig=false` 时必填） |
| `apiKey` | string? | 客户端自带密钥，仅加入出站请求头，不回传 |
| `path` | string | API 路径；`fullUrl=false` 时仅允许 `/v1/chat/completions`、`/v1/images/generations`、`/v1/audio/speech` |
| `fullUrl` | boolean | true 时 `baseUrl` 即完整请求地址（跳过路径白名单） |
| `kind` | string? | `text`/`image`/`speech`，用于服务端配置的默认值补全 |
| `body` | object | 透传给上游的请求体 |

> **注意**：`useServerConfig=false`（客户端自带端点，含 `fullUrl=true` 任意路径）等价于向任意地址发 POST 的通用代理——单用户下登录者即所有者，不再收敛；公网暴露时请配合 `ALLOW_PRIVATE_NETWORK=false`。

响应为上游响应体原样透传（上限 32MB，非 JSON 包装）。所有目标地址经过出站守卫（`ALLOW_PRIVATE_NETWORK=false` 时拒绝私网/环回/链路本地地址，含重定向逐跳检查）。

### 图片代理

```text
POST /reader3/aiProxyImage
```

请求体：`{ "url": "https://..." }`。拉取远程图片并回传（上限 20MB），用于绕开图片防盗链。URL 仅允许 http/https 且过出站守卫。
