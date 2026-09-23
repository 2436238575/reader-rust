# 书签 API

书签（阅读进度书签）的增删查接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

书签以「书名 + 作者」为唯一键（`bookName_bookAuthor`），按用户命名空间隔离存储。

## 书签对象（Bookmark）

| 字段 | 类型 | 说明 |
|------|------|------|
| `time` | number | 创建时间戳（毫秒） |
| `bookName` | string | 书名（与 `bookAuthor` 共同构成唯一键） |
| `bookAuthor` | string | 作者 |
| `chapterIndex` | number | 章节下标 |
| `chapterPos` | number | 章节内位置 |
| `chapterName` | string | 章节名 |
| `bookText` | string | 书籍摘要文本 |
| `content` | string | 书签内容 |

## 获取书签列表

```text
GET /reader3/getBookmarks
```

响应 `data` 为 `Bookmark` 数组。

## 保存书签

```text
POST /reader3/saveBookmark
```

请求体为单个 `Bookmark` 对象。`bookName` 与 `bookAuthor` 同时为空返回 400。同键已存在时覆盖。响应 `data` 为空字符串。

## 批量保存书签

```text
POST /reader3/saveBookmarks
```

请求体为 `Bookmark` 数组；`bookName`、`bookAuthor` 同时为空的书签会被静默忽略。响应 `data` 为空字符串。

## 删除书签

```text
POST /reader3/deleteBookmark
```

请求体为单个 `Bookmark` 对象，按 `bookName`+`bookAuthor` 匹配删除。响应 `data` 为空字符串。

## 批量删除书签

```text
POST /reader3/deleteBookmarks
```

请求体为 `Bookmark` 数组。响应 `data` 为空字符串。
