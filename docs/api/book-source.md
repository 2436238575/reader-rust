# 书源管理 API

书源相关的增删改查、调试与导入导出接口。除特别注明外，响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

> 例外：`readSourceFile` 成功时直接返回裸 JSON 数组（书源数组），不做统一包装。

## 获取书源列表

```text
GET /reader3/getBookSources
```

响应：

```json
{
  "isSuccess": true,
  "data": [
    {
      "bookSourceUrl": "https://example.com",
      "bookSourceName": "示例书源",
      "bookSourceGroup": "网络小说",
      "bookSourceType": 0,
      "enabled": true,
      "enabledExplore": true
    }
  ],
  "errorMsg": ""
}
```

## 获取单个书源

```text
GET /reader3/getBookSource
```

请求参数：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `bookSourceUrl` | string | 是 | 书源 URL，可放在查询参数或请求体 |

响应：`data` 为该书源对象。

## 登录书源

```text
POST /reader3/loginBookSource
```

请求参数：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `bookSourceUrl` | string | 是 | 需要登录的书源 URL |

## 获取发现页分类

```text
POST /reader3/getExploreKinds
```

请求参数：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `bookSourceUrl` | string | 否 | 书源 URL |
| `bookSource` | object | 否 | 完整的书源对象（与 `bookSourceUrl` 二选一） |

## 添加/更新书源

```text
POST /reader3/saveBookSource
```

请求体：

```json
{
  "bookSourceUrl": "https://example.com",
  "bookSourceName": "示例书源",
  "bookSourceGroup": "网络小说",
  "ruleSearch": {
    "bookList": ".book-list li",
    "name": "h3@text",
    "author": ".author@text",
    "bookUrl": "a@href"
  }
}
```

## 批量保存书源

```text
POST /reader3/saveBookSources
```

请求体为书源对象数组，或包含 `bookSourceList` / `bookSources` / `data` / `sources` 字段的对象。响应：`data` 形如 `{ "saved": true, "count": 2 }`。

## 删除书源

```text
POST /reader3/deleteBookSource
```

请求体：

```json
{
  "bookSourceUrl": "https://example.com"
}
```

## 批量删除书源

```text
POST /reader3/deleteBookSources
```

请求体为书源对象数组（按 `bookSourceUrl` 删除）。

## 删除全部书源

```text
POST /reader3/deleteAllBookSources
```

## 删除失效书源

```text
POST /reader3/deleteInvalidBookSources
```

删除被标记为失效分组（`invalid_book_sources`）的书源。响应：`data` 形如 `{ "deleted": 3 }`。

## 获取失效书源列表

```text
POST /reader3/getInvalidBookSources
```

响应：`data` 为失效书源数组。

## 测试书源

```text
POST /reader3/testBookSources
```

请求参数：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `bookSourceUrls` | string[] | 否 | 要测试的书源 URL 列表，最多 100 条；为空则测试全部 |
| `keyword` | string | 否 | 测试用搜索关键词 |
| `markInvalid` | boolean | 否 | 是否将被测失效的书源标记为失效分组，默认 true |
| `concurrent` | number | 否 | 并发数，默认 12，范围 1–12 |

响应：`data` 形如 `{ "total": 10, "valid": 8, "invalid": 2, "markedInvalid": 2, "results": [ ... ] }`。

## 远程读取书源文件

```text
POST /reader3/readRemoteSourceFile
```

请求体：`{ "url": "https://example.com/sources.json" }`。响应：`data` 为包含单个 JSON 字符串的数组（每个元素是一个书源的 JSON 序列化）。

## 本地读取书源文件

```text
POST /reader3/readSourceFile
```

上传 `multipart/form-data` 文件（`.json` 或 `.txt`），返回解析出的书源对象数组。

## 书源代理

```text
ANY /reader3/bookSourceProxy
```

查询参数：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `bookSourceUrl` | string | 是 | 书源 URL |
| `url` | string | 是 | 需要代理请求的完整或部分 URL |

将目标站点的 HTML 页面注入代理脚本后返回，使页面内相对链接、表单、脚本自动走本代理。支持 GET/POST。

## 书源客户端日志

```text
ANY /reader3/bookSourceClientLog
```

书源页面运行时的前端错误上报接口。查询参数：`message`、`source`、`lineno`、`colno`、`stack`。服务端仅记录日志，响应 `data` 为 `{ "logged": true }`。

## 书源调试（流式）

```text
GET /reader3/bookSourceDebugSSE
```

调试搜索规则的 SSE 流式接口。查询参数：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `bookSourceUrl` | string | 是 | 书源 URL |
| `keyword` | string | 否 | 调试用搜索关键词 |

## 导入书源

```text
POST /reader3/importBookSources
```

> **未实现**：批量导入书源接口，代码中无此路由。实际导入分两步——先由 `POST /reader3/readSourceFile`（本地文件）或 `POST /reader3/readRemoteSourceFile`（远程 URL）读取并解析成书源 JSON，再调 `POST /reader3/saveBookSource` / `POST /reader3/saveBookSources` 写入（依据：`src/api/router.rs`（无 import 路由））。

## 导出书源

```text
POST /reader3/exportBookSources
```

> **未实现**：书源导出接口，代码中无此路由。当前没有专门的导出接口，可由 `GET/POST /reader3/getBookSource`（单条）或 `GET/POST /reader3/getBookSources`（全部）取回书源 JSON 后自行保存为文件（依据：`src/api/router.rs`（无 export 路由））。
