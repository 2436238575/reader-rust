# 评论（章评 / 段评）

书源若声明了评论规则，阅读页会在正文里渲染段评气泡、在正文末尾渲染「本章评论」入口。**书源没声明规则时接口照常返回 200，只是 `enabled: false`**，前端据此隐藏入口。

评论地址由后端按书源规则解析，调用方只需要给出「哪本书的哪一章」。

> **缓存**：评论是会变的第三方数据（新评论、点赞数、热度排序都在动），因此这一层**保留时间过期**，默认 7 天（`REVIEW_CACHE_TTL_SECS`）。传 `refresh: 1` 可跳过缓存重新抓取。

## 通用入参

三个接口共用同一组参数（GET 查询串与 POST JSON 都支持）：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `bookUrl` | string | 是 | 书籍 URL，用作缓存归属（按书清理时用） |
| `chapterUrl` | string | 是 | 章节 URL，评论规则以这一章的正文响应为上下文求值 |
| `bookSourceUrl` | string | 否 | 书源 URL；省略时按 `bookUrl` 在书架里反查 |
| `bookSource` | object | 否 | 直接内联书源对象，优先于 `bookSourceUrl` |
| `page` | number | 否 | 页码，从 1 开始，默认 1 |
| `count` | number | 否 | 每页条数，默认 20、上限 50 |
| `refresh` | number | 否 | `1` 表示忽略缓存重新抓取 |

## 评论对象

`items` 数组里的每一项：

```json
{
  "id": "7468555045690393369",
  "name": "读者甲",
  "avatar": "https://.../a.jpg",
  "content": "评论正文",
  "time": "1717597455",
  "digg": 68,
  "replyCount": 13,
  "replies": [
    { "name": "读者乙", "content": "同感", "time": "1717597460", "replyTo": "" }
  ],
  "images": [
    "https://.../a.heic?sign=1",
    "https://.../a.jpeg?sign=2"
  ]
}
```

- `time` 是**来源站点的原始时间字符串**，后端不做归一化（有的站点是 Unix 秒，有的是 `2024-06-05 12:00`）；前端只做展示，需要排序时用站点自己的顺序。
- `digg` / `replyCount` 解析不出时是 `0`。
- `replies` 是评论下方直接展开的那几条，`replyTo` 为空表示直接回复评论本身。章评与段评都会解析内联回复。
- `images` 是评论配图，**按站点给的顺序原样返回**，不做格式过滤。站点常为同一张图给出多个变体（番茄同时给 HEIC 和 JPEG，而浏览器渲染不了 HEIC），由客户端挑自己能渲染的那个。
- `avatar` 前端不再展示（站点头像经常 403），字段保留供其他客户端使用。

## `getChapterComments` — 章评

```
GET  /reader3/getChapterComments?bookUrl=&chapterUrl=&bookSourceUrl=&page=1&count=20
POST /reader3/getChapterComments
```

```json
{
  "isSuccess": true,
  "errorMsg": "",
  "data": {
    "enabled": true,
    "data": {
      "total": 67,
      "hasMore": true,
      "page": 1,
      "items": [ /* 评论对象 */ ]
    }
  }
}
```

- `total` 是站点给出的评论总数，用于「本章评论 · N」；规则没给或取不到时退化为本页条数。
- 书源没声明章评规则时 `enabled` 为 `false`，`data.items` 为空数组，**不会请求上游**。

## `getParaCommentIndex` — 段评概览

```
GET  /reader3/getParaCommentIndex?bookUrl=&chapterUrl=&bookSourceUrl=
POST /reader3/getParaCommentIndex
```

```json
{
  "isSuccess": true,
  "errorMsg": "",
  "data": {
    "enabled": true,
    "data": {
      "paras": [
        { "paraIndex": 0, "count": 256, "text": "此时，花臂男举起了手……" },
        { "paraIndex": 5, "count": 192, "text": "“那接下来轮到我讲了。”……" }
      ]
    }
  }
}
```

- `paraIndex` 是**正文按 `\n` 切分后的行号，从 0 开始**，与站点一致。负段号（部分站点的整章聚合桶）会被过滤掉。
- `count` 是该段的评论条数。
- `text` 是段落原文（截断到 60 字）。段号会因用户自己的书源替换规则、繁简转换而漂移，前端拿它做兜底定位：位置对不上时按原文匹配。
- 列表按 `paraIndex` 升序。

## `getParaComments` — 某一段的段评

```
GET  /reader3/getParaComments?bookUrl=&chapterUrl=&paraIndex=5&page=1&count=20
POST /reader3/getParaComments
```

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `paraIndex` | number | 是 | 段号；缺失或为负返回 400 |

响应结构与章评一致（`enabled` + `total` / `hasMore` / `page` / `items`）。

## 缓存与清理

评论缓存按 `用户 / 书籍 / 章节+类型+页码` 存放于 `storage/cache/reviews/`，默认 7 天过期，同时受 `REVIEW_CACHE_USER_LIMIT_BYTES`（默认 64 MiB）容量约束。

- 删除单本书的缓存：`deleteBookCache` 会一并清掉该书的评论缓存。
- 按类型清理：`purgeCache` 传 `{ "scope": "kind", "kind": "review" }`。
- 占用统计：`cacheStats` 的返回里多了 `review` 一层。

详见 [缓存清理 API](./cache)。

## 书源规则

评论规则写在书源里（`ruleReview` 与 `ruleParaReview`），字段与求值上下文见 [书源规则参考 · 评论规则](../reference/book-source-rules#评论规则)。
