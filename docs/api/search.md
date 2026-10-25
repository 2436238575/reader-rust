# 书籍搜索 API

书籍搜索、发现页、书架与可用书源查询接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

## 书籍的身份键：`bookId`

**`bookId`（字符串，形如 `book_<n>`）是书籍的身份键**，由后端在书籍首次登记时分配，**不随换源变化**；`bookUrl` / `origin`（书源 URL）只是数据字段。搜索/发现结果**产出即登记**：响应里每条都带 `bookId` + `bookSourceId`，后续详情、目录、正文、评论等请求一律只认 `bookId`。书架、搜索结果与详情响应里的 `Book` / `SearchBook` 对象都带这两个字段。

## 搜索书籍

```text
GET /reader3/searchBook
```

查询参数：

| 参数           | 类型   | 必填 | 说明           |
| -------------- | ------ | ---- | -------------- |
| `key`          | string | 是   | 搜索关键词     |
| `page`         | number | 否   | 页码，默认 1   |
| `bookSourceId` | number | 是   | 指定书源 ID    |

响应（每条结果产出即登记，带 `bookId` 与 `bookSourceId`）：

```json
{
  "isSuccess": true,
  "data": [
    {
      "name": "书名",
      "author": "作者",
      "bookId": "book_12",
      "bookSourceId": 3,
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

也可使用 `POST /reader3/searchBook`，参数放在请求体（JSON 或 form-urlencoded）。

## 多书源搜索

```text
GET /reader3/searchBookMulti
```

查询参数：

| 参数              | 类型     | 必填 | 说明             |
| ----------------- | -------- | ---- | ---------------- |
| `key`             | string   | 是   | 搜索关键词       |
| `page`            | number   | 否   | 页码，默认 1     |
| `bookSourceIds`   | number[] | 否   | 指定多个书源 ID  |
| `bookSourceGroup` | string   | 否   | 按分组筛选书源   |

跨多书源搜索并按书名+作者合并结果（合并条目的 `bookSourceIds` 汇出所有命中的书源 ID）。

## 缓存内全文搜索

```text
GET /reader3/searchCachedContent
```

在**已缓存的章节正文**里检索关键词（可再生缓存数据，不触发任何上游请求）。与上面的
`searchBook*` 完全无关——那些是向书源发起远程搜书，这条是「在已下载的内容里找词」。
本地书走另一套存储（`data/local_books`），不在检索范围。

查询参数（也可 `POST`，JSON 或 form-urlencoded）：

| 参数         | 类型   | 必填 | 说明                                       |
| ------------ | ------ | ---- | ------------------------------------------ |
| `bookId`     | string | 是   | 书籍 ID                                    |
| `query`      | string | 是   | 纯文本子串（大小写不敏感），最长 100 字符  |
| `maxResults` | number | 否   | 命中章节数上限，默认 50，最大 200          |

响应：命中按章聚合，每章最多 3 条上下文摘要；已编辑章节读编辑版内容（与阅读器显示一致），对应命中的 `edited` 为 `true`。`coverage` 如实标注检索覆盖面
（只有缓存过的章节参与，阅读进度没走到/没手动缓存的章节不在其内）：

```json
{
  "isSuccess": true,
  "data": {
    "results": [
      {
        "chapterIndex": 2,
        "chapterTitle": "第三章",
        "chapterUrl": "https://...",
        "snippets": ["…命中词前后约 40 字符的上下文…"],
        "edited": false
      }
    ],
    "cachedChapters": 12,
    "totalChapters": 120,
    "truncated": false
  },
  "errorMsg": ""
}
```

`truncated: true` 表示没有扫完全书：命中达到上限、或超出检索资源预算（5000 章 / 50MB / 10 秒，章节按剩余预算限量读入），后面可能还有结果。

## 发现页

```text
GET /reader3/exploreBook
```

查询参数：

| 参数           | 类型   | 必填 | 说明           |
| -------------- | ------ | ---- | -------------- |
| `bookSourceId` | number | 是   | 书源 ID        |
| `ruleFindUrl`  | string | 是   | 发现页请求规则 |
| `page`         | number | 否   | 页码，默认 1   |

也可使用 `POST /reader3/exploreBook`。响应与搜索同构：每条结果产出即登记，带 `bookId` + `bookSourceId`。

## 获取书籍详情

```text
GET /reader3/getBookInfo
```

查询参数：

| 参数      | 类型   | 必填 | 说明                            |
| --------- | ------ | ---- | ------------------------------- |
| `bookId`  | string | 是   | 书籍 ID（搜索/发现结果里下发）  |
| `refresh` | number | 否   | 非 0 时绕过详情缓存强制抓取上游 |

详情结果按 `bookId` 缓存 1 小时（`refresh=1` 强刷；saveBook 与换源路径始终直抓上游），命中缓存时不产生上游请求。

响应：

```json
{
  "isSuccess": true,
  "data": {
    "name": "书名",
    "author": "作者",
    "bookId": "book_12",
    "bookSourceId": 3,
    "bookUrl": "https://...",
    "origin": "书源URL",
    "coverUrl": "https://...",
    "intro": "书籍简介",
    "kind": "分类",
    "wordCount": "字数",
    "latestChapterTitle": "最新章节",
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

| 参数      | 类型   | 必填 | 说明                |
| --------- | ------ | ---- | ------------------- |
| `bookId`  | string | 是   | 书籍 ID             |
| `refresh` | number | 否   | 非 0 时强制刷新缓存 |

请求头 `If-None-Match`（可选）：上次响应返回的 ETag（宽松比较：逗号分隔列表、
`W/` 弱前缀、`*` 均可命中）。命中时返回 **304** 空 body，客户端复用本地缓存。

目录结果按 `bookId` 缓存 1 小时（`refresh=1` 强刷），过期后重新抓取并整份覆盖。

响应：

- `200`：目录未变或客户端未带可命中的 `If-None-Match`。
- `304`：目录与 `If-None-Match` 一致，body 为空。
- 两种响应都带 `ETag`（章节列表内容的 md5 强验证器，`"<hex>"` 格式）与
  `Cache-Control: private, no-cache`（允许浏览器私有缓存，但每次使用前必须再验证）。
  目录内容变化（章节增删、标题/URL 变化）必然产生新 ETag。

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
      "bookId": "book_12",
      "bookSourceId": 3,
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

| 参数     | 类型   | 必填 | 说明    |
| -------- | ------ | ---- | ------- |
| `bookId` | string | 是   | 书籍 ID |

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
      "bookId": "book_12",
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

请求体为单个 `Book` 对象（`bookUrl` 必填，`bookSourceId` 建议带上以定位书源）。服务端先按 `bookUrl` 登记取得 `bookId`（已登记则复用），再按需补齐元数据；新入架受 `USER_BOOK_LIMIT` 上限约束（默认 2000，0 为不限），超限返回 400。响应 `data` 为保存后的书籍对象（含 `bookId` / `bookSourceId`）。

## 整架替换保存

```text
POST /reader3/saveBooks
```

两种请求体形态：

- **裸 `Book` 数组**：**整体替换**当前书架（导入场景）。替换后总数超过 `USER_BOOK_LIMIT` 时整批拒绝。
- `{ "books": [ ... ], "restore": true }`：WebDAV 恢复场景——**清表重建书籍登记表**，尊重备份自带的 `bookId`（避免恢复后书签、AI 记忆等按 `bookId` 关联的数据失配）；恢复后会顺带清理与登记表对不上的残留 AI 记忆。

响应 `data` 为保存后的数组（各项含 `bookId`）。

## 更换书籍的书源

```text
POST /reader3/setBookSource
```

请求体（JSON 或 form-urlencoded；查询参数同名也可）：

| 参数           | 类型   | 必填 | 说明                                       |
| -------------- | ------ | ---- | ------------------------------------------ |
| `bookId`       | string | 是   | 当前书籍 ID                                |
| `targetBookId` | string | 是   | 换源目标：可用书源候选自己的 `bookId`      |

目标是「查找可用书源」结果里的某个候选（搜索产出即登记，因此候选自己有 `bookId`）。换源后**当前书的 `bookId` 不变**，阅读进度、书签、AI 记忆全部保留。目标已在书架时返回 **HTTP 409**（合并会让两本书共享 `bookId`，明确拒绝）。响应 `data` 为更新后的书籍对象。

## 删除书籍

```text
POST /reader3/deleteBook
POST /reader3/deleteBooks
```

请求体分别为 `{ "bookId": "book_12" }` 与 `{ "bookIds": ["book_12", "book_13"] }`。删除时级联清理该书的章节缓存、换源候选缓存、AI 记忆、书签、登记过的图片与本地书文件。单个删除时书籍不存在返回 400；批量删除响应 `data` 为 `{ "deleted": n }`。

## 上传本地 TXT 书籍

```text
POST /reader3/uploadTxtBook
```

`multipart/form-data`，字段名 `file`。文件上限 50MB，按章节正则自动切分；单请求 multipart 字段数上限 20 个（超限 400）。受 `USER_LOCAL_BOOK_LIMIT` 约束。响应 `data` 为入架后的书籍对象（含 `bookId`）。

## 上传本地 EPUB 书籍

```text
POST /reader3/uploadEpubBook
```

`multipart/form-data`，字段名 `file`。文件上限 80MB；EPUB 条目数上限 3000，解压后总体积上限 300MB（按实际解压输出计）；单请求 multipart 字段数上限 20 个（超限 400）。受 `USER_LOCAL_BOOK_LIMIT` 约束。响应 `data` 为入架后的书籍对象（含 `bookId`）。

## 多书源搜索（流式）

```text
GET /reader3/searchBookMultiSSE
```

SSE 流式多书源搜索。查询参数：`key`、`bookSourceId`（单个书源 ID）、`bookSourceGroup`、`lastIndex`、`searchSize`（默认 50）、`concurrentCount`（默认 24）。事件里的每条结果同样带 `bookId` + `bookSourceId`。

## 按书源搜索（流式）

```text
GET /reader3/searchBookSourceSSE
```

SSE 流式的按书源查找可阅读来源接口。查询参数：`bookId`、`bookSourceGroup`、`lastIndex`、`searchSize`（默认 30）、`refresh`。

## 查找可用书源

```text
GET /reader3/getAvailableBookSource
```

查询参数：

| 参数              | 类型   | 必填 | 说明                             |
| ----------------- | ------ | ---- | -------------------------------- |
| `bookId`          | string | 是   | 书籍 ID（须在书架或已登记）      |
| `refresh`         | number | 否   | 非 0 时强制刷新                  |
| `lastIndex`       | number | 否   | 分页游标                         |
| `resultLimit`     | number | 否   | 单页结果上限，默认 20，上限 100  |
| `concurrentCount` | number | 否   | 并发数，默认 8，上限 20          |

按当前书籍的书名+作者在各书源搜索同名候选。**候选每项都带自己的 `bookId` / `bookSourceId`**（产出即登记，可直接作为 `setBookSource` 的 `targetBookId`）；**当前书源不再出现在候选里**。

响应：两种形态——带 `refresh`/`resultLimit`/`lastIndex` 任一分页参数时 `data` 为 `{ "books": [ ... ], "lastIndex": 0, "hasMore": true }`；均不带时 `data` 为裸数组 `[ ... ]`（全量结果）。

也可使用 `POST /reader3/getAvailableBookSource`。

## 查找可用书源（流式）

```text
GET /reader3/getAvailableBookSourceSSE
```

SSE 流式版本的 `getAvailableBookSource`，查询参数相同。
