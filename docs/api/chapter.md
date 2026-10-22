# 章节内容 API

章节正文获取、章节缓存与阅读进度接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

## 获取章节正文

```text
GET /reader3/getBookContent
```

查询参数：

| 参数         | 类型   | 必填 | 说明                                                              |
| ------------ | ------ | ---- | ----------------------------------------------------------------- |
| `bookId`     | string | 是   | 书籍 ID（`book_<n>`），正文缓存按它归位                           |
| `chapterUrl` | string | 是   | 章节 URL（数据字段，用来定位章节并作为缓存键 `md5(chapterUrl)`）  |
| `index`      | number | 否   | 章节下标                                                          |
| `refresh`    | number | 否   | 非 0 时强制刷新缓存                                               |

响应：`data` 为**纯字符串**（章节正文内容），不包含标题或前后章 URL：

```json
{
  "isSuccess": true,
  "data": "正文内容...",
  "errorMsg": ""
}
```

也可使用 `POST /reader3/getBookContent`。

正文可能是纯文本，也可能是 HTML（书源的正文规则吐出 HTML 时原样返回）。HTML 正文里的
`<img>` 由前端直接渲染——漫画/图集源、出版书插图、富文本正文都走这条路，不需要任何额外规则。

错误透传：

- 上游站点返回失败状态码（403/404/限流等）时，接口返回 **HTTP 502**，`errorMsg` 带真实
  状态码（如 `上游站点返回错误（HTTP 403）`）；上游连接超时/无法连接同样返回 502。
- 上游正常（2xx）但正文解析为空时：书源声明了配图规则（`ruleContentImage`）视为纯图章节
  正常返回空串；未声明则返回 **HTTP 404** + `正文为空：书源规则可能已失效，或上游页面结构有变化`。
  空正文不会进缓存。

**章节编辑版（叠加层）**：用户可以把整章正文编辑后存到自己的数据目录
（`data/<ns>/edited_chapters/`，与可再生缓存分离，purgeCache 不碰）。本章存在编辑版时
本接口优先返回编辑版（`refresh=1` 仍重抓上游更新缓存基准，但返回值以编辑版为准）；
`variant=upstream` 只读上游基准（网络书的正文缓存 / 本地书原文），不读编辑版、不抓上游，
无缓存时返回空串——这是阅读器 diff 视图的基准通道。

相关端点：

```text
POST /reader3/saveChapterEdit      { bookId, chapterUrl, chapterIndex, chapterTitle, content }
GET  /reader3/getChapterEdit       ?bookId&chapterUrl（无编辑版时 data 为 null）
GET  /reader3/listChapterEdits     ?bookId → { urls: string[] }
POST /reader3/deleteChapterEdit    { bookId, chapterUrl }（删除即恢复上游版本）
POST /reader3/diffChapterEdit      { base: string[], edited: string[] }（前端切好的段落数组）
```

`diffChapterEdit` 用服务器上的系统 git 计算段落 diff（`git diff --no-index`），返回
`{ fallback: false, ops: [...] }`（与前端 `ParagraphDiffOp` 同构：equal/add/del/modify +
段下标与文本）。系统没有 git（或执行失败/超时）时返回 `{ fallback: true }` 并在服务端日志
警告一次，前端据此回退到本地 LCS 计算。单侧段落数上限 2 万、单段 8KB。

单章内容上限 2MB；删书时编辑版目录随书级联删除。

## 获取章节配图

```text
GET /reader3/getChapterImages
```

书源声明了配图规则（`ruleContentImage`）时，返回本章的配图列表；没声明时 `enabled: false`。
配图规则见[书源规则规格 · 章节配图](/reference/book-source-rules#_17-7-章节配图规则-rulecontentimage)。

查询参数与 `getBookContent` 同族（也支持 `POST`）：

| 参数         | 类型   | 必填 | 说明     |
| ------------ | ------ | ---- | -------- |
| `bookId`     | string | 是   | 书籍 ID  |
| `chapterUrl` | string | 是   | 章节 URL |

响应：

```json
{
  "isSuccess": true,
  "errorMsg": "",
  "data": {
    "enabled": true,
    "images": [
      {
        "url": "https://p3-reading-sign.fqnovelpic.com/novel-pic-r/857a72fe.jpeg?x-expires=1884944022&x-signature=...",
        "caption": "配图（画师：奈月Oo）",
        "paraIndex": 100,
        "width": 1400,
        "height": 933
      }
    ]
  }
}
```

- `url`：**本站取图地址**（`/reader3/image/<id>`），不是书源原始地址——书源地址带时效签名，
  收口到图片管道后签名过期也不影响（见「获取图片（封面等）」）。
- `paraIndex`：插入位置，等于**正文按 `\n` 切分后的行号**（从 0 开始，插在该行之前）；
  为 `null` 时排在章末。
- `caption`：图片说明；正文里已有同一句话时前端不重复显示。
- `width` / `height`：原始尺寸，站点没给就是 `0`。
- **无配图不是错误**：`enabled: true` + `images: []` 表示「这一章没有配图」；
  `enabled: false` 才表示「书源不支持配图」，前端据此决定是否渲染配图区。
- **不缓存**：图片地址普遍带时效签名（`x-expires`），缓存下来过一阵就是死链，因此每次开章现取。

## 缓存章节（流式）

```text
GET /reader3/cacheBookSSE
```

SSE 流式缓存指定书籍的章节正文。查询参数：

| 参数              | 类型   | 必填 | 说明                         |
| ----------------- | ------ | ---- | ---------------------------- |
| `bookId`          | string | 是   | 书籍 ID                      |
| `tocUrl`          | string | 否   | 起始章节 URL，从该章开始缓存 |
| `count`           | number | 否   | 缓存章节数，默认 0 表示全部  |
| `refresh`         | number | 否   | 非 0 时忽略已有缓存          |
| `concurrentCount` | number | 否   | 并发数，默认 24              |

也可使用 `POST /reader3/cacheBookSSE`。

## 删除章节缓存

```text
POST /reader3/deleteBookCache
```

删除粒度是整本书：正文缓存、目录缓存与该书的换源候选缓存一并清理（评论缓存与书籍详情缓存不动）。

查询参数或请求体：

| 参数     | 类型   | 必填 | 说明    |
| -------- | ------ | ---- | ------- |
| `bookId` | string | 是   | 书籍 ID |

响应：`data` 形如 `{ "deleted": true, "contentCache": true, "chapterListCache": true }`。

## 保存阅读进度

```text
POST /reader3/saveBookProgress
```

请求参数（查询或请求体）：

| 参数       | 类型   | 必填 | 说明             |
| ---------- | ------ | ---- | ---------------- |
| `bookId`   | string | 是   | 书籍 ID          |
| `index`    | number | 是   | 当前章节下标     |
| `position` | number | 否   | 正文中阅读位置   |

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

## 获取图片（封面等）

```text
GET /reader3/image/<id>?accessToken=<jwt>
```

书源里的图片地址普遍是**带时效签名的短命 URL**（番茄图床的 `x-expires` / `x-signature`，
签名还会随宿主会话轮换），而且格式不一定是浏览器能渲染的（番茄封面是 HEIC，Chrome 直接报错）。
后端因此把图片统一收口到这一层：

- **id = `md5(去掉查询串的地址)`**：同一张图永远同一个 id，前端只拿 id、不接触书源地址。
  书籍响应里的 `coverUrl` 会被自动改写成本接口路径。
- **首次取图时抓上游并落盘**：HEIC/HEIF 转成 JPEG，其余格式原样缓存（类型走位图白名单，
  `text/html`、`image/svg+xml` 一律降级为 `application/octet-stream`）。
- **之后长期命中本地缓存**（`Cache-Control: public, max-age=31536000, immutable`）：
  签名过期不影响已经抓下来的图。
- **签名失效会自愈**：抓取失败时，用登记时留下的书籍上下文（`book_id`）重新求值一次书源的封面地址再试。
- 映射与图片本体落在 `<STORAGE_DIR>/cache/image/`：`<id>.json` 是记录（上游地址 + 所属书籍的 `book_id`，供按书清理与回源自愈），
  `<id>.bin` 是图片字节；目录本身即「id → 记录」的映射表。

单个图片最大 32MiB；目录容量上限与封面共用 `CACHE_COVER_LIMIT_BYTES`（超限按最旧优先淘汰）。
上游失败、id 未登记、出站守卫拦截都返回 404。目标地址同样过出站守卫。

## 获取封面（兼容入口）

```text
GET /reader3/cover?path=<封面URL>
```

`customCoverUrl`（用户自定义封面）等历史入口存的还是书源地址，这个接口保留兼容：把 `path` 登记成 id
后走上面同一条管道（转码、缓存、自愈一致）。**需要登录**，令牌的携带方式与
`/reader3/image/<id>` 相同（`Authorization` 头或 `accessToken` 查询参数）。书源下发的封面地址（`coverUrl`）在响应里已被改写成 `/reader3/image/<id>`，正常调用方不需要走这个入口。

## 获取本地 EPUB 资源

```text
GET /reader3/localEpubAsset?bookId=<book_n>&path=<包内路径>
```

读取已导入本地 EPUB 的包内资源（封面图、样式等）。`bookId` 必须是本地 EPUB 书（`bookUrl` 为 `local-epub:` 前缀）；`path` 只能命中该书 manifest 中登记的资源，否则 404。响应为资源字节流（带对应 Content-Type）。
