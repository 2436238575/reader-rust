# 章节内容 API

章节正文获取、章节缓存与阅读进度接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

## 获取章节正文

```text
GET /reader3/getBookContent
```

查询参数：

| 参数         | 类型   | 必填 | 说明                                                                 |
| ------------ | ------ | ---- | -------------------------------------------------------------------- |
| `bookId`     | string | 是   | 书籍 ID（`book_<n>`）                                                |
| `chapterUrl` | string | 是   | 章节 URL（章节身份，正文版本按它归档）                               |
| `index`      | number | 否   | 章节下标                                                             |
| `refresh`    | number | 否   | 非 0 时同步回源一次；失败回退已有缓存                               |
| `variant`    | string | 否   | `upstream` 只预览最新上游；缺省/`default` 按服务端选择解析默认版本  |

默认版本解析顺序：**当前用户手动选择的版本 > 最近一次用户版本（编辑/手动导入）>
最新上游版本**。用户版本优先于上游；`variant=upstream` 只是本次预览，不改变选择。

响应 `data` 除了正文，还带当前版本定位信息：

```json
{
  "isSuccess": true,
  "errorMsg": "",
  "data": {
    "content": "编辑后的整章正文...",
    "edited": true,
    "variant": "default",
    "versionId": 42,
    "versionKind": "edited",
    "baseVersionId": 40,
    "diffBaseVersionId": 41,
    "latestUserVersionId": 42,
    "latestUpstreamVersionId": 41,
    "upstreamChanged": true,
    "selectionSource": "edited",
    "stale": false,
    "diff": {
      "ops": [
        { "type": "equal", "baseIndex": 0, "editedIndex": 0 },
        { "type": "modify", "baseIndex": 1, "editedIndex": 1, "baseText": "旧句", "editedText": "新句" },
        { "type": "del", "baseIndex": 2, "baseText": "被删掉的整段" },
        { "type": "add", "editedIndex": 3, "editedText": "新增的一段" }
      ]
    }
  }
}
```

- `edited`：当前返回的是否用户版本（编辑版或手动导入）。
- `variant`：`default` 或 `upstream`。
- `versionId` / `versionKind` / `baseVersionId`：当前版本的标识、来源，以及编辑时基于哪一版；`baseVersionId` 只作历史信息。
- `diffBaseVersionId`：本次 `diff` 使用的基准版本，始终是最新上游版本。
- `latestUserVersionId` / `latestUpstreamVersionId`：最近一次用户版本与最新上游。
- `upstreamChanged`：用户版本的基准之后，上游是否又变过。
- `selectionSource`：`selected`（手动选择）、`edited`（最近用户版本）、`upstream`
  （没有用户版本）或 `upstream_preview`（显式预览上游）。
- `stale`：当前返回的是旧数据——到期未回源，或最近一次回源失败。
- `diff.ops`：**当前用户版本相对最新上游版本（`diffBaseVersionId`）的改动**，
  段落级操作序列，`type` 为 `equal` / `add` / `del` / `modify`，带 `baseIndex`
  与 `editedIndex`。评论源只对最新上游正文取段号，所以阅读页的 diff 必须和
  最新上游对齐。为省流量，`equal` 行不带文本（前端用 `content` 补齐）；
  `add` 只带 `editedText`，`del` 只带 `baseText`，`modify` 两段都带。
- `diffError`：算不出差异时的原因。此时 `diff` 缺省，但正文照常可用。常见原因：
  最新上游版本不可读、章节过大。diff 由服务端进程内计算（imara-diff），
  不依赖任何外部工具。

`diff` 始终是「最新上游 → 当前用户版本」，包含上游在编辑之后发生的变化。
只看任意两个历史版本的差异，用
[`getChapterVersionDiff`](#版本时间线与对比)。`variant=upstream` 时 `diff` 缺省。

回源规则：

- 读取时距上次回源尝试超过 `CONTENT_REVALIDATE_TTL_SECS`（默认 24 小时），服务端会在
  **后台回源**，当前请求先返回缓存；下一次读取看到新版本。
- `refresh=1` 同步回源；成功则追加版本，失败回退已有缓存并标记 `stale`。
- 没有缓存时同步抓取，失败按错误返回。
- 上游成功但正文为空：已有缓存视为失败并回退；没有缓存时，书源声明了配图规则
  （`ruleContentImage`）视为纯图章节返回空串，否则返回 **HTTP 404** +
  `正文为空：书源规则可能已失效，或上游页面结构有变化`。
- 正文版本严格保留：刷新不会删除旧版本，编辑也不会覆盖旧版本。

错误透传：

- 上游站点返回失败状态码（403/404/限流等）时，接口返回 **HTTP 502**，`errorMsg` 带真实
  状态码（如 `上游站点返回错误（HTTP 403）`）；上游连接超时/无法连接同样返回 502。
- 有缓存时的后台回源/强刷失败不阻断阅读，响应带 `stale: true`。

也可使用 `POST /reader3/getBookContent`。正文可能是纯文本，也可能是 HTML（书源的正文规则
吐出 HTML 时原样返回）。HTML 正文里的 `<img>` 由前端直接渲染——漫画/图集源、出版书插图、
富文本正文都走这条路，不需要任何额外规则。

## 批量获取章节正文（预加载）

```text
POST /reader3/getBookContents
```

一次请求读取最多 10 章正文，供前端预加载附近章节使用。请求体：

| 参数        | 类型   | 必填 | 说明                                                        |
| ----------- | ------ | ---- | ----------------------------------------------------------- |
| `bookId`    | string | 否   | 网络书必填；本地 TXT/EPUB（`chapterUrl` 自带 `local-` 前缀）可缺省 |
| `chapters`  | array  | 是   | 1 到 10 条；每项 `{ "index": number, "chapterUrl": string }`，`chapterUrl` 必填 |
| `variant`   | string | 否   | 透传给每章，语义同单章接口                                   |

```json
{
  "bookId": "book_1",
  "chapters": [
    { "index": 2, "chapterUrl": "https://example.com/c3" },
    { "index": 3, "chapterUrl": "https://example.com/c4" }
  ]
}
```

响应 `data` 是与请求同序的数组，每章独立成败——**整体始终 HTTP 200**：

```json
{
  "isSuccess": true,
  "errorMsg": "",
  "data": [
    { "index": 2, "data": { "content": "...", "edited": false, "...": "同 getBookContent 的 data" }, "error": null },
    { "index": 3, "data": null, "error": "上游站点返回错误（HTTP 403）" }
  ]
}
```

- `data` 与单章 `getBookContent` 的 `data` 完全同构（共用同一条读取链路）。
- 某章失败只写该条的 `error`（文案与单章接口失败时的 `errorMsg` 一致），其余章节照常返回。
- `chapters` 为空、超过 10 条或某条缺 `chapterUrl` 时整批拒绝，返回 **HTTP 400**（不做半执行）。
- 章节读取并发上限 4，不会打满上游；不支持 `refresh`——预加载不需要强刷，超期章节与单章
  接口一样由后台回源。
- 前端阅读器已接入：预加载把当前章前后最多 3 章合成一次批量请求，单章失败跳过，
  整批失败（断网/401/400）回退逐章 `getBookContent`，行为与不接入批量时一致。

### 编辑版与版本历史

用户编辑和手动导入都进同一个正文池，作为时间线里的用户版本；删除当前用户版本只清指针，
历史版本仍然保留。单章内容上限 16MB。

```text
POST /reader3/saveChapterEdit      { bookId, chapterUrl, chapterIndex, chapterTitle, content,
                                     baseVersionId?, note? }
GET  /reader3/getChapterEdit       ?bookId&chapterUrl（当前用户版本；没有时 data 为 null）
GET  /reader3/listChapterEdits     ?bookId → { urls: string[] }（当前显示用户版本的章节）
POST /reader3/deleteChapterEdit    { bookId, chapterUrl }（清除当前用户版本，回落上游）
GET/POST /reader3/getChapterTimeline
        ?bookId&chapterUrl&limit?&beforeVersionId? → { items, latestUpstreamVersionId,
                                                      latestUserVersionId, selectedVersionId,
                                                      effectiveVersionId }
GET/POST /reader3/getChapterVersion
        ?bookId&chapterUrl&versionId → { versionId, kind, baseVersionId?, createdAt,
                                        rawSize, chars, note, bookSourceName, content }
GET/POST /reader3/getChapterVersionDiff
        ?bookId&chapterUrl&fromVersionId&toVersionId → { fromVersionId, toVersionId, ops }
POST /reader3/selectChapterVersion { bookId, chapterUrl, versionId? }
        （versionId 为空表示「跟随最新」，删除手动选择）
POST /reader3/resetBookToUpstream { bookId } → { cleared }
        （整书换回最新上游：清掉本书全部版本选择与用户版本指针，每章显示
        回落到最新上游，本地书回到原文；历史版本保留。cleared 为摘掉
        用户版本指针的章节数）
POST /reader3/importChapterVersion { bookId, chapterUrl, content, chapterIndex?,
                                     chapterTitle?, baseVersionId?, note?,
                                     makeCurrent?, createdAt? }
POST /reader3/previewChapterImport  multipart: bookId, mode?, chapterUrl?, chapterIndex?,
                                     chapterTitle?, file? | text?
        → { mode, total, matched, unmatched, empty, changed, cached, uncached,
            previewId, chapters: [...] }
POST /reader3/importChapterText     multipart: 同上 + kind?, note?, makeCurrent?,
                                     createdAt?, selectedIndexes?, previewId?
        → { mode, selected, imported, unchanged, skipped, failed, errors }
POST /reader3/getChapterImportDiff { previewId, sourceIndex }
        → { changed, ops, diffError, currentExcerpt, currentChars, importChars }
```

- `saveChapterEdit` 默认把新编辑设为当前；`baseVersionId` 不传时取当前最新上游。
- `getChapterTimeline` 按版本时间（`createdAt`）倒序，同一时间再按 `versionId`；`kind` 为 `upstream` / `edited` / `imported`；
  `isCurrent` / `isLatestUpstream` / `isLatestUser` 标识版本状态；每条带 `chars`（正文字数）。
- `selectChapterVersion` 按用户保存到服务端，跨设备一致；“预览”不写状态。
- `importChapterVersion` 默认只进时间线（`makeCurrent=false`），可指定历史时间与备注。

#### 单章 / 整本 TXT 导入

`previewChapterImport` 只解析和匹配，不写库；`importChapterText` 按预览结果批量写入。
两个接口都收 `multipart/form-data`，正文可以放在 `file` 文件字段，也可以放在 `text` 文本字段。
文件支持 UTF-8、GB18030 与 UTF-16（含 BOM），上限 50MB。

- `mode`：`book` 按章节标题拆分（标题形如「第 12 章」「第一百零二回」），`single` 把整段文本
  作为 `chapterUrl` 对应的当前章节，`auto` 为默认值——能拆出多章按整本处理，否则按单章处理。
- 单章模式的正文如果第一行是章节标题，会去掉标题行后入库。
- 整本模式的匹配顺序是「标题完全一致 → 章节序号相同 → 按位置对齐」。目录本身带章节序号时，
  正文里带序号却匹配不上的章节不参与按位置对齐，避免把「第 37 章」硬塞进某一章。
- 目录里的卷名条目（`isVolume`）不参与匹配。空章节和未匹配章节不会写入。
- 预览返回的 `chapters[]` 每项包含 `sourceIndex`、`title`、`chars`、`excerpt`、`status`
  （`matched` / `empty` / `unmatched`）、`matchKind`（`title` / `number` / `order` / `single` / `none`），
  以及匹配到的 `chapterIndex`、`chapterUrl`、`chapterTitle`。
- 预览会拿每章和**当前显示的正文**比对：`cached` 表示有正文可比，`changed` 表示内容不同。
  当前返回的是用户版本时比用户版本，没有用户版本时比最新上游；没有缓存的章节 `cached=false`、
  `changed=false`，前端默认不勾选并提示先缓存全书。比对口径与 diff 面板一致：剥 HTML 标签、
  去掉空行和行首尾空白，所以只差 HTML 包裹或空行的同一份正文不算变化。
- 前端默认只勾选 `matched && cached && changed` 的章节；「全选已匹配」可以在确认后覆盖所有章节。
- `previewId` 是预览暂存的短期令牌（进程内缓存 30 分钟，最多 3 份、单份 24MB），
  用于详情对比与导入复用。`getChapterImportDiff` 按 `sourceIndex` 取回该章与当前正文的段落 diff；
  正文过大时 `previewId` 为 `null`，此时详情只展示正文片段。
- `importChapterText` 的 `selectedIndexes` 是逗号分隔的 `sourceIndex`：不传表示导入全部已匹配章节，
  传空字符串表示一章都不导入。`kind` 为 `edited`（默认）或 `imported`；`makeCurrent` 默认 false。
- 导入带上 `previewId` 时直接复用预览暂存的切章/匹配结果执行写入，**不用再传 `file`/`text`**
  （同一份正文不二次上传）；暂存条目被导入消费后仍保留到过期，失败可拿同一个 `previewId` 重试。
  `previewId` 未命中、已过期或不属于当前书籍时返回 400（提示重新解析）；不带 `previewId` 的旧调用
  方式照旧：重新解析、重新匹配一次，需要和预览时提交同一份正文与 `mode`。
- **空正文不写入时间线**：文本源（`bookSourceType` 不是 2）返回空正文按失败处理；图片源
  （`bookSourceType=2`）允许空正文，但只更新检查时间，不追加 0 字节的上游版本。存量部署里
  已有的空上游版本不会自动删除，需要在维护窗口按 `docs/maintainers/database.md` 的说明清理。

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

删除粒度是整本书：目录缓存与该书的换源候选缓存一并清理；**正文历史不删**，
评论缓存与书籍详情缓存也不动。

查询参数或请求体：

| 参数     | 类型   | 必填 | 说明    |
| -------- | ------ | ---- | ------- |
| `bookId` | string | 是   | 书籍 ID（须已登记，否则 404） |

响应：`data` 形如 `{ "deleted": true, "contentCache": false, "chapterListCache": true }`。

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
GET /reader3/image/<id>?w=<像素宽>
```

浏览器里 `<img>` 会自动携带 HttpOnly Cookie `reader_access`，URL 不需要再拼令牌；API 客户端仍可用 `Authorization: Bearer <jwt>`。其他路径的 `?accessToken=` 查询参数已不再接受。

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
- 映射与图片本体存放在 `<STORAGE_DIR>/cache/image/`：`<id>.json` 是记录（上游地址 + 所属书籍的 `book_id`，供按书清理与回源自愈），
  `<id>.bin` 是图片字节；目录本身即「id → 记录」的映射表。

### 按宽度缩放（`w`）与 webp 协商（`Accept`）

查询参数 `w` 是**目标像素宽**：把图缩小到该宽度（等比，Triangle 滤镜），用于封面/章节配图
按展示尺寸取图省流。规则：

- 取值范围 clamp 到 `[64, 2048]`，**只降不升**——`w` 大于等于原图宽度时返回原图尺寸；
- 请求头 `Accept` 含 `image/webp` 时输出**有损 webp（质量 80）**，否则缩放结果编码为
  **JPEG（质量 85）**；响应都带 `Vary: Accept`（同一 URL 有两种表示，缓存须按 Accept 区分）；
- 只有 JPEG/PNG/BMP 会参与缩放与转码；**GIF（动画）/WebP/AVIF/SVG/ICO 原样透传**，
  避免 `w` / Accept 破坏动画与矢量；
- 单边尺寸超出编码器上限（webp 16383 像素、JPEG 65535 像素）的极端长图不转码，
  **回退原图直出**（缩放后的目标尺寸也在检查范围内）；
- 不带 `w` 且 `Accept` 不含 webp 时返回原图，行为与旧版完全一致；
- 派生图落盘为 `<id>.w<实际宽度>.<webp|jpg>`，与原图同目录、共用 `CACHE_COVER_LIMIT_BYTES`
  容量上限与最旧优先淘汰，命中后不再重复编码。派生图随原图重抓落盘（上游地址失效自愈、
  本体被容量淘汰后重取）与按书删除一并清理，之后的请求按新原图按需重建。

单个图片最大 32MiB；上游失败、id 未登记、出站守卫拦截都返回 404。目标地址同样过出站守卫。

## 获取封面（兼容入口）

```text
GET /reader3/cover?path=<封面URL>&w=<像素宽>
```

`customCoverUrl`（用户自定义封面）等历史入口存的还是书源地址，这个接口保留兼容：把 `path` 登记成 id
后走上面同一条管道（转码、缓存、自愈一致），同样支持 `w` 缩放与 `Accept` 的 webp 协商。**需要登录**，
浏览器由 HttpOnly Cookie 携带会话，
API 客户端用 `Authorization` 头；`?accessToken=` 查询参数已不再接受。书源下发的封面地址（`coverUrl`）在响应里已被改写成 `/reader3/image/<id>`，正常调用方不需要走这个入口。

## 获取本地 EPUB 资源

```text
GET /reader3/localEpubAsset?bookId=<book_n>&path=<包内路径>
```

读取已导入本地 EPUB 的包内资源（封面图、样式等）。`bookId` 必须是本地 EPUB 书（`bookUrl` 为 `local-epub:` 前缀）；`path` 只能命中该书 manifest 中登记的资源，否则 404。响应为资源字节流（带对应 Content-Type）。
