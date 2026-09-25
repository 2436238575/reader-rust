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

## 获取章节配图

```text
GET /reader3/getChapterImages
```

书源声明了配图规则（`ruleContentImage`）时，返回本章的配图列表；没声明时 `enabled: false`。
配图规则见[书源规则规格 · 章节配图](/reference/book-source-rules#_17-7-章节配图规则-rulecontentimage)。

查询参数与 `getBookContent` 同族（也支持 `POST`）：

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `bookUrl` | string | 是 | 书籍 URL（别名 `url`） |
| `chapterUrl` | string | 是 | 章节 URL（别名 `href`） |
| `bookSourceUrl` | string | 否 | 书源 URL（别名 `origin`） |
| `bookSource` | object | 否 | 直接内联书源对象，优先于 `bookSourceUrl` |

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

## 获取封面

```text
GET /reader3/cover?path=<封面URL>
```

抓取远程封面并缓存返回。**匿名可访问**（固定使用隔离的 `public` 命名空间，不携带任何用户书源会话 Cookie）。

行为约定：

- 响应 `Content-Type` 收敛为位图白名单（jpeg/png/webp/gif/avif/bmp/ico）；上游返回 `text/html`、`image/svg+xml` 等不可渲染为位图的类型时一律降级为 `application/octet-stream`（防同源脚本执行）
- 单个封面最大 32MiB；缓存目录容量上限 256MiB，超限按最旧优先淘汰
- 上游失败/被出站守卫拦截时统一返回 404
- 目标 URL 经出站守卫校验（`ALLOW_PRIVATE_NETWORK=false` 时拒绝私网/环回/链路本地地址）

## 获取本地 EPUB 资源

```text
GET /reader3/localEpubAsset?bookUrl=<local-epub:...>&path=<包内路径>
```

读取已导入本地 EPUB 的包内资源（封面图、样式等）。`bookUrl` 必须是 `local-epub:` 前缀的本地书；`path` 只能命中该书 manifest 中登记的资源，否则 404。响应为资源字节流（带对应 Content-Type）。
