# 缓存管理 API

缓存的显式清理与占用统计。

后端缓存大多**不按时间过期**：内容一旦抓取即持续有效，直到被显式清理或超出容量上限被淘汰。按时间过期的例外是章节列表与书籍详情（1 小时 TTL）与评论（7 天 TTL）。`purgeCache` 是显式回收手段，`cacheStats` 用来确认清理效果。

## 缓存分层

缓存键全部以身份键为准：书籍维度一律用 `bookId`（`book_<n>`），不再取 URL 的 MD5。

| 层               | 位置                                                          | 说明                                          |
| ---------------- | ------------------------------------------------------------- | --------------------------------------------- |
| `content`        | `storage/cache/<ns>/<bookId>/<md5(chapterUrl)>.txt`           | 章节正文，按书一个目录                        |
| `cover`          | `storage/cache/image/<md5(去查询串地址)>.{bin,json}`          | 书源图片（封面、章节配图、评论配图）统一存放，全局共享；`.json` 记录含 `book_id`，供按书清理与回源自愈 |
| `chapterList`    | `storage/data/<ns>/chapters/<bookId>.json`                    | 章节列表，1 小时 TTL                          |
| `searchResults`  | `storage/data/<ns>/book_sources/<bookId>.json`                | 某本书在各书源中的换源候选搜索结果            |
| `review`         | `storage/cache/reviews/<ns>/<bookId>/…`                       | 章评/段评，7 天 TTL                           |
| `invalidSources` | `storage/cache/invalid_book_sources/<ns>.json`                | 失效书源清单                                  |
| `bookInfo`       | `storage/cache/bookinfo/<ns>/<bookId>.json`                   | 书籍详情，1 小时 TTL（`refresh=1` 强刷）      |

容量上限由 `CACHE_USER_LIMIT_BYTES`（默认 512MiB）与 `CACHE_COVER_LIMIT_BYTES`（默认 256MiB，图片目录：封面与配图共用）控制，超出时按修改时间最旧优先淘汰；设为 `0` 表示不限制。

## 清理缓存

```text
POST /reader3/purgeCache
```

请求体（`scope` 缺省为 `user`）：

```json
{
  "scope": "user",
  "username": "目标用户（可选）",
  "bookId": "book_12",
  "kind": "content"
}
```

| `scope` | 语义                                                           | 需要的字段                  |
| ------- | -------------------------------------------------------------- | --------------------------- |
| `user`  | 清理该用户的全部缓存                                           | `username` 可选，缺省为自己 |
| `book`  | 清理单本书的正文、章节列表、书籍详情、换源候选与评论缓存，连带清该书登记的图片 | `bookId` 必填               |
| `kind`  | 只清理该用户的某一层                                           | `kind` 必填                 |
| `all`   | 清理全部缓存                                                   | 无                          |

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

`scope=book` 时正文与换源候选按「整本/整个文件」计数，因此最多为 1；`cover` 计入该书登记过的图片条数。

只清单本书的正文、章节列表与换源候选（不动评论/详情/图片）也可以继续用 [`deleteBookCache`](./chapter#删除章节缓存)。

## 缓存占用统计

```text
GET /reader3/cacheStats
GET /reader3/cacheStats?all=true
GET /reader3/cacheStats?username=<其他用户>
```

| 参数       | 说明                      |
| ---------- | ------------------------- |
| `username` | 目标命名空间；缺省为自己  |
| `all`      | `true` 时汇总全部命名空间 |

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

图片缓存（`cover` 层）是全局共享目录，不在任何用户命名空间下：无论按用户统计还是按层清理，都会一并处理这份图片目录；统计与清理还会计入旧版本遗留的 `storage/cache/<ns>/cover/`。`all=true` 时统计的是全部命名空间的合计。
