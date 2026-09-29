# 书籍搜索 API

书籍搜索、发现页、书架与可用书源查询接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

## 搜索书籍

```text
GET /reader3/searchBook
```

查询参数：

| 参数            | 类型   | 必填 | 说明                                                                                                           |
| --------------- | ------ | ---- | -------------------------------------------------------------------------------------------------------------- |
| `key`           | string | 是   | 搜索关键词                                                                                                     |
| `page`          | number | 否   | 页码，默认 1                                                                                                   |
| `bookSourceUrl` | string | 否   | 指定书源 URL（别名 `origin`，仅 form-urlencoded POST 请求体接受别名；query 与 JSON body 请用 `bookSourceUrl`） |

响应：

```json
{
  "isSuccess": true,
  "data": [
    {
      "name": "书名",
      "author": "作者",
      "coverUrl": "https://...",
      "bookUrl": "https://...",
      "intro": "简介",
      "kind": "玄幻",
      "wordCount": "100万字",
      "lastChapter": "最新章节",
      "origin": "书源URL"
    }
  ],
  "errorMsg": ""
}
```

也可使用 `POST /reader3/searchBook`，参数放在请求体。

## 多书源搜索

```text
GET /reader3/searchBookMulti
```

查询参数：

| 参数              | 类型     | 必填 | 说明             |
| ----------------- | -------- | ---- | ---------------- |
| `key`             | string   | 是   | 搜索关键词       |
| `page`            | number   | 否   | 页码，默认 1     |
| `bookSourceUrls`  | string[] | 否   | 指定多个书源 URL |
| `bookSourceGroup` | string   | 否   | 按分组筛选书源   |

跨多书源搜索并按书名+作者合并结果。

## 发现页

```text
GET /reader3/exploreBook
```

查询参数：

| 参数            | 类型   | 必填 | 说明                      |
| --------------- | ------ | ---- | ------------------------- |
| `ruleFindUrl`   | string | 是   | 发现页请求规则            |
| `page`          | number | 否   | 页码，默认 1              |
| `bookSourceUrl` | string | 否   | 书源 URL（别名 `origin`） |

## 获取书籍详情

```text
GET /reader3/getBookInfo
```

查询参数：

| 参数            | 类型   | 必填 | 说明                            |
| --------------- | ------ | ---- | ------------------------------- |
| `url`           | string | 是   | 书籍详情页 URL                  |
| `bookSourceUrl` | string | 否   | 书源 URL（别名 `origin`）       |
| `refresh`       | number | 否   | 非 0 时绕过详情缓存强制抓取上游 |

详情结果按用户命名空间缓存 10 分钟（`refresh=1` 强刷；saveBook 与换源路径始终直抓上游），命中缓存时不产生上游请求。

响应：

```json
{
  "isSuccess": true,
  "data": {
    "name": "书名",
    "author": "作者",
    "coverUrl": "https://...",
    "intro": "书籍简介",
    "kind": "分类",
    "wordCount": "字数",
    "lastChapter": "最新章节",
    "tocUrl": "目录页URL"
  },
  "errorMsg": ""
}
```

也可使用 `POST /reader3/getBookInfo`。

## 获取目录

```text
GET /reader3/getChapterList
```

查询参数：

| 参数            | 类型   | 必填 | 说明                      |
| --------------- | ------ | ---- | ------------------------- |
| `tocUrl`        | string | 否   | 目录页 URL                |
| `bookUrl`       | string | 否   | 书籍 URL（别名 `url`）    |
| `bookSourceUrl` | string | 否   | 书源 URL（别名 `origin`） |
| `refresh`       | number | 否   | 非 0 时强制刷新缓存       |

响应：

```json
{
  "isSuccess": true,
  "data": [
    {
      "title": "第一章 xxx",
      "url": "https://...",
      "index": 0
    }
  ],
  "errorMsg": ""
}
```

也可使用 `POST /reader3/getChapterList`。

## 获取书架

```text
GET /reader3/getBookshelf
```

响应：`data` 为书籍对象数组（`Book` 模型，字段为 camelCase 序列化名）：

```json
{
  "isSuccess": true,
  "data": [
    {
      "name": "书名",
      "author": "作者",
      "bookUrl": "https://...",
      "origin": "https://书源",
      "coverUrl": "https://...",
      "tocUrl": "https://...",
      "intro": "简介",
      "kind": "分类",
      "wordCount": "字数",
      "latestChapterTitle": "最新章节",
      "totalChapterNum": 100,
      "durChapterIndex": 0,
      "durChapterPos": 50,
      "durChapterTitle": "当前阅读章节标题",
      "canUpdate": true,
      "updateTime": "2024-01-01"
    }
  ],
  "errorMsg": ""
}
```

其他可能存在的字段：`originName`、`customCoverUrl`、`charset`、`canReName`、`downloadUrls`、`lastCheckTime`、`type`、`group`、`infoHtml`、`tocHtml` 等（依据：`src/model/book.rs`）。

## 获取书架中单本书

```text
GET /reader3/getShelfBook
```

查询参数：

| 参数  | 类型   | 必填 | 说明     |
| ----- | ------ | ---- | -------- |
| `url` | string | 是   | 书籍 URL |

响应：`data` 为单个书籍对象（字段同上）。也可使用 `POST /reader3/getShelfBook`。

## 获取书架（含缓存信息）

```text
GET /reader3/getShelfBookWithCacheInfo
```

与 `getBookshelf` 相同，但每项书籍对象额外返回 `cachedChapterCount`（number，已缓存章节数）。

```json
{
  "isSuccess": true,
  "data": [
    {
      "name": "书名",
      "bookUrl": "https://...",
      "cachedChapterCount": 12
    }
  ],
  "errorMsg": ""
}
```

依据：`src/api/handlers/book.rs` 的 `get_shelf_book_with_cache_info`（插入 `cachedChapterCount`）。

## 保存书籍到书架

```text
POST /reader3/saveBook
```

请求体为单个 `Book` 对象（`origin`、`bookUrl` 必填）。已存在同书（按书名/作者/URL 匹配）时更新并保留阅读进度等字段；新入架受 `USER_BOOK_LIMIT` 上限约束（默认 2000，0 为不限），超限返回 400。响应 `data` 为保存后的书籍对象。

## 整架替换保存

```text
POST /reader3/saveBooks
```

请求体为 `Book` 数组，**整体替换**当前书架（导入场景）。替换后总数超过 `USER_BOOK_LIMIT` 时整批拒绝。响应 `data` 为保存后的数组。

## 更换书籍的书源

```text
GET /reader3/setBookSource
```

查询参数：`bookUrl`（别名 `url`）、`newUrl`、`bookSourceUrl`。也可 `POST`（JSON 或 form-urlencoded 请求体）。响应 `data` 为更新后的书籍对象。

## 删除书籍

```text
POST /reader3/deleteBook
POST /reader3/deleteBooks
```

请求体分别为单个 `Book` 与 `Book` 数组。删除时级联清理该书的书籍缓存、AI 记忆与本地书文件。单个删除时书籍不存在返回 400；批量删除响应 `data` 为 `{ "deleted": n }`。

## 上传本地 TXT 书籍

```text
POST /reader3/uploadTxtBook
```

`multipart/form-data`，字段名 `file`。文件上限 50MB，按章节正则自动切分。受 `USER_LOCAL_BOOK_LIMIT` 约束。响应 `data` 为入架后的书籍对象。

## 上传本地 EPUB 书籍

```text
POST /reader3/uploadEpubBook
```

`multipart/form-data`，字段名 `file`。文件上限 80MB；EPUB 条目数上限 3000，解压后总体积上限 300MB（按实际解压输出计）。受 `USER_LOCAL_BOOK_LIMIT` 约束。响应 `data` 为入架后的书籍对象。

## 多书源搜索（流式）

```text
GET /reader3/searchBookMultiSSE
```

SSE 流式多书源搜索。查询参数：`key`、`bookSourceUrl`、`bookSourceGroup`、`lastIndex`、`searchSize`（默认 50）、`concurrentCount`（默认 24）。

## 按书源搜索（流式）

```text
GET /reader3/searchBookSourceSSE
```

SSE 流式的按书源查找可阅读来源接口。查询参数：`url`、`bookSourceGroup`、`lastIndex`、`searchSize`（默认 30）、`refresh`。

## 查找可用书源

```text
GET /reader3/getAvailableBookSource
```

查询参数：

| 参数              | 类型   | 必填 | 说明                             |
| ----------------- | ------ | ---- | -------------------------------- |
| `url`             | string | 否   | 书籍 URL                         |
| `name`            | string | 否   | 书名                             |
| `author`          | string | 否   | 作者                             |
| `origin`          | string | 否   | 书源 URL（别名 `bookSourceUrl`） |
| `refresh`         | number | 否   | 非 0 时强制刷新                  |
| `lastIndex`       | number | 否   | 分页游标                         |
| `resultLimit`     | number | 否   | 单页结果上限，默认 20，上限 100  |
| `concurrentCount` | number | 否   | 并发数，默认 8，上限 20          |

响应：两种形态——带 `refresh`/`resultLimit`/`lastIndex` 任一分页参数时 `data` 为 `{ "books": [ ... ], "lastIndex": 0, "hasMore": true }`；均不带时 `data` 为裸数组 `[ ... ]`（全量结果）。

也可使用 `POST /reader3/getAvailableBookSource`。

## 查找可用书源（流式）

```text
GET /reader3/getAvailableBookSourceSSE
```

SSE 流式版本的 `getAvailableBookSource`。
