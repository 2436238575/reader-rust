# 缓存管理 API

缓存的显式清理与占用统计。

后端缓存**不按时间过期**：内容一旦抓取即持续有效，直到被显式清理或超出容量上限被淘汰。因此 `purgeCache` 是唯一的回收手段，`cacheStats` 用来确认清理效果。

## 缓存分层

| 层 | 位置 | 说明 |
|----|------|------|
| `content` | `storage/cache/<ns>/<md5(bookUrl)>/*.txt` | 章节正文，按书一个目录 |
| `cover` | `storage/cache/public/cover/*` | 封面图片，匿名抓取，固定 `public` 命名空间 |
| `chapterList` | `storage/data/<ns>/chapters/<md5(tocUrl)>.json` | 章节列表 |
| `searchResults` | `storage/data/<ns>/book_sources/<md5(bookUrl)>.json` | 某本书在各书源中的搜索结果 |
| `invalidSources` | `storage/cache/invalid_book_sources/<ns>.json` | 失效书源清单 |
| `bookInfo` | `storage/cache/bookinfo/<ns>/<md5(bookUrl)>.json` | 书籍详情，10 分钟 TTL（唯一例外，`refresh=1` 强刷） |

容量上限由 `CACHE_USER_LIMIT_BYTES`（默认 512MiB）与 `CACHE_COVER_LIMIT_BYTES`（默认 256MiB，封面目录）控制，超出时按修改时间最旧优先淘汰；设为 `0` 表示不限制。

## 清理缓存

```text
POST /reader3/purgeCache
```

请求体（`scope` 缺省为 `user`）：

```json
{
  "scope": "user",
  "username": "目标用户（可选）",
  "bookUrl": "https://example.com/book/1",
  "kind": "content"
}
```

| `scope` | 语义 | 需要的字段 |
|---------|------|-----------|
| `user` | 清理该用户的全部缓存 | `username` 可选，缺省为自己 |
| `book` | 清理单本书的正文、章节列表、搜索结果与评论 | `bookUrl` 必填 |
| `kind` | 只清理该用户的某一层 | `kind` 必填 |
| `all` | 清理全部缓存 | 无 |

`kind` 取值：`content`、`cover`、`chapterList`、`searchResults`、`review`（`chapterList` 同时清书籍详情缓存）。

响应 `data` 为各层被删除的**文件数**：

```json
{
  "isSuccess": true,
  "data": {
    "purged": {
      "content": 128,
      "cover": 0,
      "chapterList": 2,
      "searchResults": 1,
      "review": 0,
      "invalidSources": 0
    }
  },
  "errorMsg": ""
}
```

`scope=book` 时正文与搜索结果按「整本/整个文件」计数，因此最多为 1。

单本书的正文缓存也可以继续用 [`deleteBookCache`](./chapter#删除书籍缓存)，语义等价。

## 缓存占用统计

```text
GET /reader3/cacheStats
GET /reader3/cacheStats?all=true
GET /reader3/cacheStats?username=<其他用户>
```

| 参数 | 说明 |
|------|------|
| `username` | 目标命名空间；缺省为自己 |
| `all` | `true` 时汇总全部命名空间 |

响应 `data` 为各层的文件数与字节数：

```json
{
  "isSuccess": true,
  "data": {
    "content": { "files": 128, "bytes": 4194304 },
    "cover": { "files": 12, "bytes": 524288 },
    "chapterList": { "files": 2, "bytes": 8192 },
    "searchResults": { "files": 1, "bytes": 2048 }
  },
  "errorMsg": ""
}
```

封面缓存固定落在 `public` 命名空间，因此按用户统计时只有 `public` 用户会看到非零的 `cover`；`all=true` 时统计的就是这个全局封面目录。
