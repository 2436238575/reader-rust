# RSS 订阅 API

RSS 源管理与文章获取接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

## 获取 RSS 源列表

```text
GET /reader3/getRssSources
```

响应：

```json
{
  "isSuccess": true,
  "data": [
    {
      "sourceUrl": "https://example.com/feed.xml",
      "sourceName": "RSS源名称",
      "enabled": true
    }
  ],
  "errorMsg": ""
}
```

## 添加 RSS 源

```text
POST /reader3/saveRssSource
```

请求体：

```json
{
  "sourceUrl": "https://example.com/feed.xml",
  "sourceName": "RSS源名称",
  "enabled": true
}
```

## 批量保存 RSS 源

```text
POST /reader3/saveRssSources
```

请求体为 RSS 源对象数组（按 `sourceUrl` 更新或插入）。

## 删除 RSS 源

```text
POST /reader3/deleteRssSource
```

请求体：

```json
{
  "sourceUrl": "https://example.com/feed.xml"
}
```

## 批量删除 RSS 源

```text
POST /reader3/deleteRssSources
```

请求体为 RSS 源对象数组（按 `sourceUrl` 删除）。响应：`data` 形如 `{ "deleted": 1 }`。

## 远程读取 RSS 源文件

```text
POST /reader3/readRemoteRssSourceFile
```

请求体：`{ "url": "https://example.com/rss.json" }`。响应：`data` 为包含单个 JSON 字符串的数组。

## 本地读取 RSS 源文件

```text
POST /reader3/readRssSourceFile
```

上传 `multipart/form-data` 文件（`.json` 或 `.txt`），返回解析出的 RSS 源对象数组。

## 获取 RSS 文章列表

```text
GET /reader3/getRssArticles
```

请求体（或参数）：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `sourceUrl` | string | 是 | RSS 源 URL |
| `sortName` | string | 否 | 分类名称 |
| `sortUrl` | string | 否 | 分类链接，默认同 `sourceUrl` |
| `page` | number | 否 | 页码，默认 1 |

响应：`data` 形如 `{ "first": [ ...文章 ], "second": null }`，其中 `first` 为当页文章数组（`RssArticle` 模型，camelCase，依据：`src/model/rss.rs:35`）：

| 字段 | 类型 | 说明 |
|------|------|------|
| `origin` | string | 来源 URL |
| `sort` | string | 分类名 |
| `title` | string | 文章标题 |
| `order` | number | 排序时间戳 |
| `link` | string | 文章链接 |
| `pubDate` | string? | 发布时间 |
| `description` | string? | 摘要 |
| `content` | string? | 正文 |
| `image` | string? | 题图 |
| `read` | boolean? | 是否已读 |
| `variable` | string? | 扩展变量 |

也可使用 `POST /reader3/getRssArticles`。

## 获取 RSS 内容

```text
GET /reader3/getRssContent
```

请求体（或参数）：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `sourceUrl` | string | 是 | RSS 源 URL |
| `link` | string | 是 | 文章链接 |
| `origin` | string | 是 | 文章来源 |

响应：`data` 为文章正文的 HTML 字符串。

也可使用 `POST /reader3/getRssContent`。
