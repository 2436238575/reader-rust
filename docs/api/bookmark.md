# 书签 API

书签（阅读进度书签）的增删查接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

书签的身份是 **`bookId` + `chapterIndex` + `chapterPos`** 三元组：同一本书同一位置视为同一条书签（重复保存覆盖），**一本书可以有多条书签**（修复了旧版以「书名+作者」为键、一书只能存一签的覆盖 bug）。按用户命名空间隔离存储；`bookName` / `bookAuthor` 只是展示快照，不参与判等。删除书籍时该书的书签会被联动删除。

## 书签对象（Bookmark）

| 字段           | 类型   | 说明                                   |
| -------------- | ------ | -------------------------------------- |
| `bookId`       | string | 所属书籍的 `book_<n>`（身份键之一）    |
| `time`         | number | 创建时间戳（毫秒）                     |
| `bookName`     | string | 书名（展示快照，不参与判等）           |
| `bookAuthor`   | string | 作者（展示快照）                       |
| `chapterIndex` | number | 章节下标（身份键之一）                 |
| `chapterPos`   | number | 章节内位置（身份键之一）               |
| `chapterName`  | string | 章节名                                 |
| `bookText`     | string | 书籍摘要文本                           |
| `content`      | string | 书签内容                               |

## 获取书签列表

```text
GET /reader3/getBookmarks
```

响应 `data` 为 `Bookmark` 数组。

## 保存书签

```text
POST /reader3/saveBookmark
```

请求体为单个 `Bookmark` 对象。`bookId` 为空返回 400。同身份（`bookId` + `chapterIndex` + `chapterPos`）已存在时覆盖。响应 `data` 为空字符串。

## 批量保存书签

```text
POST /reader3/saveBookmarks
```

请求体为 `Bookmark` 数组；`bookId` 为空的书签会被静默忽略。响应 `data` 为空字符串。

## 删除书签

```text
POST /reader3/deleteBookmark
```

请求体为单个 `Bookmark` 对象，按身份三元组（`bookId` + `chapterIndex` + `chapterPos`）匹配删除。响应 `data` 为空字符串。

## 批量删除书签

```text
POST /reader3/deleteBookmarks
```

请求体为 `Bookmark` 数组，逐条按身份三元组匹配删除。响应 `data` 为空字符串。
