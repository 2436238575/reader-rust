# 章节内容 API

章节正文获取、章节缓存与阅读进度接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

## 获取章节正文

```text
GET /reader3/getBookContent
```

查询参数：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `chapterUrl` | string | 是 | 章节 URL（别名 `url`、`href`） |
| `bookSourceUrl` | string | 否 | 书源 URL（别名 `origin`） |
| `index` | number | 否 | 章节下标（当传入书籍 URL 时定位章节） |
| `refresh` | number | 否 | 非 0 时强制刷新缓存 |

响应：

```json
{
  "isSuccess": true,
  "data": {
    "title": "章节标题",
    "content": "正文内容...",
    "nextUrl": "下一章URL",
    "prevUrl": "上一章URL"
  },
  "errorMsg": ""
}
```

也可使用 `POST /reader3/getBookContent`。

## 缓存章节（流式）

```text
GET /reader3/cacheBookSSE
```

SSE 流式缓存指定书籍的章节正文。查询参数：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `url` | string | 是 | 书籍 URL（别名 `bookUrl`） |
| `tocUrl` | string | 否 | 起始章节 URL，从该章开始缓存 |
| `count` | number | 否 | 缓存章节数，默认 0 表示全部 |
| `refresh` | number | 否 | 非 0 时忽略已有缓存 |
| `concurrentCount` | number | 否 | 并发数，默认 24 |

也可使用 `POST /reader3/cacheBookSSE`。

## 删除章节缓存

```text
POST /reader3/deleteBookCache
```

查询参数或请求体：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `bookUrl` | string | 是 | 书籍 URL（别名 `url`） |
| `chapterUrl` | string | 否 | 章节 URL |
| `url` | string | 否 | 书籍 URL（与 `bookUrl` 等效） |

响应：`data` 形如 `{ "deleted": true, "contentCache": true, "chapterListCache": true }`。

## 保存阅读进度

```text
POST /reader3/saveBookProgress
```

请求参数（查询或请求体）：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `url` | string | 是 | 书籍 URL（别名 `bookUrl`、`searchBook.bookUrl`） |
| `index` | number | 是 | 当前章节下标 |
| `position` | number | 否 | 正文中阅读位置 |

响应 `data` 为空字符串。

## 缓存章节内容

```text
POST /reader3/cacheBookContent
```

> **未实现**：该接口尚未在代码中实现（依据：`src/api/router.rs` 仅有 `cacheBookSSE`，查无 `cacheBookContent` 路由）。

## 获取阅读进度

```text
GET /reader3/getReadProgress
```

> **未实现**：该接口尚未在代码中实现（依据：`src/api/router.rs` 查无 `getReadProgress` 路由；现仅有 `POST /reader3/saveBookProgress` 保存进度，无独立获取进度的接口）。

## 保存阅读进度（旧名）

```text
POST /reader3/saveReadProgress
```

> **未实现**：该接口尚未在代码中实现（依据：`src/api/router.rs` 查无 `saveReadProgress` 路由；正确路径为 `POST /reader3/saveBookProgress`）。
