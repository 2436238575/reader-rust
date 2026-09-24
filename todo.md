# TODO — 实现状态清单

> 整理自 2026-09-22/23 两轮后端审计与「文档 vs 代码」全量比对（四路并行、逐条代码实证）。
> **2026-09-24 已逐项处理**：下面的「规格差异」每一项都做了决策——要么补齐实现（见「本轮补齐」），
> 要么接受分叉并写进 `docs/reference/book-source-rules.md` 的 `> **reader-rust 实现差异**` 标注。
> 本文件现在只剩三类内容：**已接受的分叉**、**未实现的功能面**、**已知残余风险**。

## 已实现（规格主要面）

- 规则引擎六种用途（搜索/发现/详情/目录/正文/下一章）与五种解析方式（CSS/JSONPath/XPath/正则/JS）
- 组合规则 `&&`（拼接）/ `||`（首个非空）/ `%%`（交错）：**CSS、XPath、JSONPath、元素级字段、列表正则**均支持；
  切分跳过引号/括号/花括号内分隔符；列表正则的 `&&` 为逐级下钻
- 字段级模式前缀 `@css:` / `@json:` / `@xpath:` / `@regex:` / `@js:`（`<js>`）
- XPath 支持普通 HTML 页面（严格 XML 解析失败时自动经 html5ever 容错转换）
- URL 处理全链路：`@js:`/`<js>` URL、`{{}}` 内联 JS、`{key}`/`{page}` 占位符、`<1,2,3>` 页码候选、参数 JSON（method/headers/body/charset/retry/type）、书源级 header
- AllInOne 正则（`$1..$99` 捕获）、`##` 替换（含第四段 replaceFirst、缺 replacement 即删除匹配）、旧字段迁移（`ruleSearchList` 等）
- 索引语法：新式 `[0,2,-1]` / `[1:5:2]` / `[!0,-1]` 与旧式 `!0:2`、`.1:5:2`（区间与步长）
- JS 运行时：`java.ajax/get/post/put`、`md5Encode`、`timeFormat`、base64、AES-CBC 解密、encodeURI 系列、`now`/`uuid`、`cache.get/put`、`kv_get/kv_put`、`cookie.removeCookie`、`java.log/toast/openUrl`（空操作）；jsLib（远程 URL + 内联）
- loginCheckJs（每次抓取后执行，可改写响应）、`@put`/`@get` 临时变量（值规则支持逗号与冒号）
- 多用户隔离（命名空间 + 独立 Cookie jar/JS KV）、WebDAV 备份、本地 TXT/EPUB、RSS、AI 记忆/代理、章节文件缓存（容量淘汰）
- 章评/段评（`ruleReview` / `ruleParaReview`，含配图与内联回复）与 7 天评论缓存
- 安全基线：SSRF 出站守卫（含重定向逐跳 DNS 复检）、JS 沙箱资源上限、Argon2id 密码哈希、登录/WebDAV 限速、路径穿越防护、各类体积上限

## 本轮（2026-09-24）补齐的规格差异

| 项 | 结论 | 代码锚点 |
|----|------|---------|
| JsonPath 组合符 | 补齐：列表与字段两条路径都支持 `&&`/`||`/`%%` | `jsonpath_query_combined`、`combine_strings` |
| 列表项字段组合符 | 补齐：CSS 与 JSON 元素级字段都能组合 | `eval_field_json_with_ctx`、`eval_field_html_with_ctx` |
| 字段级模式切换 | 补齐：元素级新增 `@xpath:`/`@json:`/`@regex:`；文档级 `@json:` 生效 | 同上、`eval_literal_field` |
| 列表正则 `&&` 下钻 | 补齐：多段逐级下钻，最后一段产出捕获组 | `regex_list_captures` |
| 旧式索引区间 | 补齐：`!0:2`、`.1:5:2` 按区间与步长处理 | `parse_legacy_index_item` |
| `##` 第四段 | 补齐：第四段即 replaceFirst（取首个匹配片段）；缺 replacement 时删除匹配 | `apply_legado_regex` |
| `isTrue()` 词表 | 补齐：加入 `not`（`none`/`off` 作为超集保留） | `is_truthy` |
| `bookUrlPattern` | 补齐：真正参与匹配，命中即按详情页解析 | `book_url_pattern_matches` |
| 取值后缀 | 补齐：`html`/`all` 先移除 script/style 再取 outer HTML；`textNodes` 只取直接文本节点 | `strip_script_style`、`direct_text_nodes` |
| `kind` 多值 | 补齐：全部命中按 `,` 连接（规则含 JS/模板时退回单值） | `eval_kind_*`、`join_multi_values` |
| 搜索结果去重 | 补齐：按「书名\|作者」去重保序，先于反转 | `dedupe_books` |
| JSON 搜索 `bookUrl` 回退 | 补齐：为空时回退 `baseUrl` | `search_books_json` |
| `formatJs` 的 `gInt` | 补齐：初值 0，同一轮格式化的章节之间复用 | `eval_js_with_bindings_and_globals`、`apply_toc_format_js` |
| `@put` 值切分 | 补齐：顶层 `,`/`:` 才切分，引号括号内的保留 | `split_put_map` |
| `java.*` 缺口 | 补齐：`log`/`toast`/`openUrl` 注册为空操作，避免整条规则抛错 | `src/parser/js.rs` 注册表 |
| `exploreUrl` JS 缓存 | 补齐：按 `MD5(bookSourceUrl + exploreUrl)` 缓存 1 小时 | `cached_explore_script` |
| 书源正则编译缓存 | 补齐：编译结果（含失败）缓存，带条目上限 | `compiled_regex` |
| JS KV / jsLib 缓存上限 | 补齐：超上限整表清空，避免无界增长 | `kv_put_scoped`、`js_lib_script` |
| `retry` 非法值 | 补齐：解析失败时保留默认次数，不再退化成 0 | `src/crawler/url_analyzer.rs` |
| JS 顶层 `return` | 补齐：首次求值报 `return not in a function` 时包成 IIFE 重试，表达式风格不受影响 | `eval_script` |
| `source` 绑定 | 补齐：`source` 是真实书源对象（`bookSourceUrl`/`bookSourceName`/`header`…），`{{source.bookSourceUrl}}` 可正常拼 URL | `with_book_source`、`src/parser/js.rs` |
| `<js>` 链式求值 | 补齐：`</js>` 之后的片段继续对 JS 结果求值（JSON 按 JSON 规则、否则按 HTML 文档规则）；`@js:` 仍吞掉后续 | `extract_js`、`eval_rule_on_text` |

## 接受分叉（已在规格 `docs/reference/book-source-rules.md` 标注）

| 项 | 分叉内容 |
|----|---------|
| 切分器细节 | `{}` 计入平衡组、引号内 `\` 视为转义（比规格宽容，改动会破坏 `{{}}` 模板与 JS 正则） |
| `concurrentRate` | 严格 `limit` 次/窗口，不复刻 `limit + 1` 边界 |
| `formatBookName` / `formatBookAuthor` / `wordCountFormat` | 未实现：模型里没有这三个字段 |
| `preUpdateJs` | 总是执行（无 `runPerJs` 字段），语义是目录解析前的 body 预处理；无 `java.reGetBook()`/`refreshTocUrl()` |
| 目录排序去重 | 逐页解析时去重并保留首个，不实现按书反转的 `reverseToc` |
| `nextContentUrl` | 单链顺序跟随（取第一条），不做多 URL 并发；有环检测与跨站/下一章拦截 |
| `java.*` 方法面 | 缺 `ajaxAll`/`connect`/`importScript`/`cacheFile`/`getCookie`/`downloadFile`/`hex`/`htmlFormat`/简繁/`queryTTF`/`toNumChapter`/`toURL` 等，调用会抛错 |
| `book` / `chapter` 绑定 | 仅 `formatJs` 提供真实对象，其余规则上下文是空对象占位（`source` 已是真实书源） |
| `webJs` | 无 WebView，直接在抓取到的 body 上执行 |

## 未实现（文档已按约定标注「未实现」）

- 书源登录流程：`loginUi` 表单、登录脚本 `login()` 调用、AES 登录信息、登录头合并（规格 §11.1/§12.1 已标注）
- WebView 渲染抓取（`webView`/`webJs`/`webViewDelayTime` 仅解析不执行，规格 §5.7 已标注）
- 批量导入/导出书源接口（`docs/api/book-source.md` 已标注，可用 readSourceFile + saveBookSources 两步替代）
- `register` 独立注册接口（注册走 `login` + `isLogin=false`）
- `cacheBookContent` / `getReadProgress` / `saveReadProgress`（`docs/api/chapter.md` 已标注）
- `getTxtTocRules` 恒返回空数组（已标注）
- 保留字段无消费方：`enabledCookieJar`、`coverDecodeJs`、`exploreScreen`
  （`ruleReview` 已在本项目接通，见章评/段评）

## 新发现（本轮之外，待决策）

- 搜索结果去重键用「书名|作者」（与多书源合并一致）；规格用的是对象 `equals`，两者在多版本同书场景下略有差异。

## 已知残余风险（安全）

- 出站守卫 DNS 校验与实际连接分离（TOCTOU），理论可 DNS rebinding；需连接层 IP 钉扎，reqwest 未暴露
- 登录限速为进程内状态（多实例部署需共享存储）
- 进程内小缓存（正则/JS KV/jsLib/exploreUrl）都有条目上限，但清空是整表操作，极端情况下会退化为反复重算
- 公网部署要点：`ALLOW_PRIVATE_NETWORK=false`，并按需配置 `INVITE_CODE` 与 `USER_LIMIT`
  （旧的 `SECURE` 开关已随 JWT 重构删除）

## 配额默认值

| 配置 | 默认 | 状态 |
|------|------|------|
| `USER_LIMIT` | 50 | 已实现 |
| `USER_BOOK_LIMIT` | 2000 | 已实现（2026-09-23 接入 saveBook/saveBooks） |
| `USER_LOCAL_BOOK_LIMIT` | 0（不限） | 已实现 |
