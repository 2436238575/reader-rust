# TODO — 实现状态清单

> 整理自 2026-09-22/23 两轮后端审计与「文档 vs 代码」全量比对（四路并行、逐条代码实证）。
> 每行标注状态；「规格差异」指与 `docs/reference/book-source-rules.md`（阅读3.0 兼容规格）的偏离，逐项决策「补实现」或「接受分叉并在规格标注」。

## 已实现（规格主要面）

- 规则引擎六种用途（搜索/发现/详情/目录/正文/下一章）与五种解析方式（CSS/JSONPath/XPath/正则/JS）
- 组合规则 `&&`（拼接）/ `||`（首个非空）/ `%%`（交错）：CSS、XPath、JS 均支持；切分跳过引号/括号内分隔符
- XPath 支持普通 HTML 页面（严格 XML 解析失败时自动经 html5ever 容错转换）
- URL 处理全链路：`@js:`/`<js>` URL、`{{}}` 内联 JS、`{key}`/`{page}` 占位符、`<1,2,3>` 页码候选、参数 JSON（method/headers/body/charset/retry/type）、书源级 header
- AllInOne 正则（`$1..$99` 捕获）、`##` 替换（含 `###` 整串首个扩展）、旧字段迁移（`ruleSearchList` 等）
- JS 运行时：`java.ajax/get/post/put`、`md5Encode`、`timeFormat`、base64、AES-CBC 解密、encodeURI 系列、`now`/`uuid`、`cache.get/put`、`kv_get/kv_put`、`cookie.removeCookie`；jsLib（远程 URL + 内联）
- loginCheckJs（每次抓取后执行，可改写响应）、`@put`/`@get` 临时变量
- 多用户隔离（命名空间 + 独立 Cookie jar/JS KV）、WebDAV 备份、本地 TXT/EPUB、RSS、AI 记忆/代理、章节文件缓存（TTL+容量淘汰）
- 安全基线：SSRF 出站守卫（含重定向逐跳 DNS 复检）、JS 沙箱资源上限、Argon2id 密码哈希、登录/WebDAV 限速、路径穿越防护、各类体积上限

## 未实现（文档已按约定标注「未实现」）

- 书源登录流程：`loginUi` 表单、登录脚本 `login()` 调用、AES 登录信息、登录头合并（规格 §11.1/§12.1 已标注）
- WebView 渲染抓取（`webView`/`webJs`/`webViewDelayTime` 仅解析不执行，规格 §5.7 已标注）
- 批量导入/导出书源接口（`docs/api/book-source.md` 已标注，可用 readSourceFile + saveBookSources 两步替代）
- `register` 独立注册接口（注册走 `login` + `isLogin=false`）
- `cacheBookContent` / `getReadProgress` / `saveReadProgress`（`docs/api/chapter.md` 已标注）
- `getTxtTocRules` 恒返回空数组（已标注）
- 保留字段无消费方：`enabledCookieJar`、`coverDecodeJs`、`exploreScreen`、`ruleReview`

## 规格差异（逐项决策：补实现 / 接受分叉）

### 影响中

| 项 | 规格说法 | 代码实际 |
|----|---------|---------|
| JsonPath 组合符 | `getString`/`getStringList` 支持 `&&`/`\|\|`/`%%` | 未切分，组合写法直接查空 |
| 旧式索引区间 | `tag.li!0:2` 的 `:` 支持区间与步长 | 按离散下标 `{0,2}` 处理；区间用新式 `[0:2]` |
| `##` 第四段 | replaceFirst 取首个匹配片段 | 尾段被忽略；整串首个替换用扩展写法 `###` |
| `isTrue()` 词表 | `false/no/not/0` 为假 | 词表 `0/false/null/none/no/off`，缺 `not` 多 `none`/`off` |
| `bookUrlPattern` | 命中即按详情页解析/入库匹配 | 仅做空值门控，从不做正则匹配 |
| 列表项字段组合符 | 元素级字段可用 `&&`/`\|\|`/`%%` | 仅文档级字段支持 |
| 字段级模式切换 | 每条规则可独立 XPath/Json/CSS | 元素级仅支持 `@css:`/`@js:`；文档级 `@xpath:` 支持、`@json:` 不支持 |
| `nextContentUrl` | 多 URL 并发 | 单链跟随（取第一条） |
| `java.*` 方法面 | ajaxAll/connect/importScript/cacheFile/getCookie/downloadFile/hex/htmlFormat/简繁/queryTTF/toNumChapter/toURL/toast/log/openUrl 等 | 未实现；注意 `java.log` 等调用会让整条 JS 抛错 |
| exploreUrl JS 缓存 | 按 `MD5(bookSourceUrl+exploreUrl)` 缓存 | 每次重新执行，无缓存 |
| `webJs` 正文预处理 | 在 WebView 中执行 | 无 WebView，仅在抓取后直接对 body 执行（若非 webView 场景） |

### 影响低

- `formatJs` 缺 `gInt` 绑定（`index` 从 1 起一致）
- `preUpdateJs`：规格要求 `runPerJs=true` 才执行且提供 `java.reGetBook()`/`refreshTocUrl()`；实现为每次目录解析都执行、返回值直接当新 body
- `@put` 仅在整条规则以 `@put:` 开头时生效；值按裸 `,`/`:` 切分（含逗号的值规则会切坏）
- 字段后处理 `formatBookName`/`formatBookAuthor`/`wordCountFormat` 未实现；`kind` 多值只取首个；搜索结果不去重
- JSONPath 路径下 `bookUrl` 为空时不回退 baseUrl（HTML/JS 路径会回退）
- `concurrentRate` 边界：实现严格 `limit` 次/窗口（规格允许 limit+1）
- `retry` 默认 2 且只对 5xx/网络错误退避（规格未写缺省）
- 目录排序去重：去重保留首个重复项（规格为两次反转+一次去重）
- 规则模式下引号内 `\` 一律转义（规格为不转义）；`{}` 也计入平衡组
- 取值后缀：`html`/`all` 为 inner HTML 且不去 script/style；`textNodes` 为递归后代文本
- 列表正则不支持 `&&` 多段逐级下钻（单条 pattern）
- JS 绑定：`book`/`chapter` 为占位空对象（仅 formatJs 有真实 chapter），`speakText`/`speakSpeed` 不注入

## 已知残余风险（安全）

- 出站守卫 DNS 校验与实际连接分离（TOCTOU），理论可 DNS rebinding；需连接层 IP 钉扎，reqwest 未暴露
- 书源正则无编译缓存（编译成本有 crate 上限兜底）
- `JS_KV`/`JS_LIB_CACHE` 为无界 map（单次求值受 JS 堆上限约束，增速慢）
- 登录限速为进程内状态（多实例部署需共享存储）
- `SECURE=false` 时注册开放、出站守卫默认放行内网——公网部署必须 `SECURE=true` 并按需配置 `INVITE_CODE`

## 配额默认值

| 配置 | 默认 | 状态 |
|------|------|------|
| `USER_LIMIT` | 50 | 已实现 |
| `USER_BOOK_LIMIT` | 2000 | 已实现（2026-09-23 接入 saveBook/saveBooks） |
| `USER_LOCAL_BOOK_LIMIT` | 0（不限） | 已实现 |
