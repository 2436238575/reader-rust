# JavaScript 规则

Reader-Rust 的 JS 规则运行在服务端 QuickJS 中，不是浏览器页面环境。

## 基本语法

支持三种写法：

| 写法           | 说明                                   |
| -------------- | -------------------------------------- |
| `js:...`       | 整条规则就是 JS                        |
| `@js:...`      | 先按普通规则取值，再把结果交给 JS      |
| `<js>...</js>` | 与 `@js:` / `js:` 等价，取决于放置位置 |

示例：

```json
{
  "author": ".author@text@js:input.replace(/^作者[:：]?/, '').trim()"
}
```

## 可用变量

| 变量             | 类型   | 说明                                      |
| ---------------- | ------ | ----------------------------------------- |
| `input`          | string | 当前输入值                                |
| `result`         | string | `input` 的别名                            |
| `src`            | string | `input` 的别名                            |
| `base_url`       | string | 当前基础 URL                              |
| `baseUrl`        | string | `base_url` 的别名                         |
| `url`            | string | 默认等于 `base_url`                       |
| `key`            | string | 搜索关键词，仅 URL 规则里通常有值         |
| `page`           | number | 页码，仅 URL 规则里通常有值               |
| `source`         | object | 当前书源简化上下文，含 `key` / `getKey()` |
| `cache`          | object | `get(key)` / `put(key, value)`            |
| `cookie`         | object | 当前只有 `removeCookie(key)` 占位方法     |
| `java`           | object | 一组 Legado 风格辅助方法                  |
| `book`           | object | 预留对象，默认空                          |
| `chapter`        | object | 预留对象，默认空                          |
| `title`          | string | 预留值，部分场景会覆盖                    |
| `nextChapterUrl` | string | 预留值，默认空                            |
| `rssArticle`     | object | 预留对象，默认空                          |

`formatJs` 这类带绑定的规则还会额外注入上下文变量，例如 `index`、`chapter`。

## `java` / 全局辅助函数

### `java`

| 方法                                                                   | 说明                                          |
| ---------------------------------------------------------------------- | --------------------------------------------- |
| `java.ajax(spec)`                                                      | 发起请求，`spec` 形如 `url,{"method":"POST"}` |
| `java.get(url)` / `java.post(url, body)` / `java.put(url, body)`       | 简单 HTTP 请求                                |
| `java.md5Encode(text)`                                                 | MD5                                           |
| `java.timeFormat(ts)`                                                  | 时间格式化                                    |
| `java.androidId()` / `java.deviceID()`                                 | 设备 ID                                       |
| `java.base64Encode(text)` / `java.base64Decode(text)`                  | Base64                                        |
| `java.encodeURIComponent(text)` / `java.decodeURIComponent(text)`      | URL 编码                                      |
| `java.encodeURI(text)` / `java.decodeURI(text)`                        | URL 编码                                      |
| `java.aesBase64DecodeToString(input, key, "AES/CBC/PKCS5PADDING", iv)` | AES-CBC 解密（Base64 输入）                   |
| `java.now()`                                                           | 当前毫秒时间戳                                |
| `java.uuid()`                                                          | UUID                                          |

### 全局函数

| 方法                                     | 说明         |
| ---------------------------------------- | ------------ |
| `kv_get(key)` / `kv_put(key, value)`     | 共享 KV      |
| `regex_replace(input, pattern, replace)` | 正则替换     |
| `strip_ws(input)`                        | 清理空白字符 |

## 常用操作

### 字符串处理

```js
input.replace(/pattern/g, '')
input.match(/chapter_(\d+)/)?.[1] || ''
new URL(input, baseUrl).href
input.split('\n').filter(Boolean)
```

### 数组返回

```json
{
  "chapterList": "js:[{ chapterName: '第一章', chapterUrl: '/1' }, { chapterName: '第二章', chapterUrl: '/2' }]"
}
```

`js:` 规则返回数组或对象时，解析器会自动把结果转成 JSON 字符串，再交给后续阶段消费。

### URL 规则

::: v-pre

```json
{
  "searchUrl": "/search?wd={{java.encodeURIComponent(key)}}&page={{page}}"
}
```

:::

### 目录标题格式化

```json
{
  "ruleToc": {
    "formatJs": "`${index}. ${title}`"
  }
}
```

### 复用 `jsLib`

```json
{
  "jsLib": "function cleanAuthor(v){ return v.replace(/^作者[:：]?/, '').trim(); }",
  "ruleSearch": {
    "author": ".author@text@js:cleanAuthor(input)"
  }
}
```

`jsLib` 也可以是 JSON 对象：值为 `http(s)://` 的条目会远程拉取，**其他字符串值直接当内联 JS 执行**。
注意每次规则求值都会重建 JS 运行时——jsLib 里不要依赖跨规则的共享状态。

## 当前限制

- 没有 `document`、`window`、`Element` 这类浏览器 DOM API
- 不要依赖浏览器控制台
- 如果需要操作 HTML 结构，优先先用 CSS / XPath / JSONPath 缩小范围，再让 JS 处理字符串
- JS 比普通选择器慢，能不用就不用

资源与出站约束（防恶意书源，超限按失败/空值处理）：

- 单次求值上限：128MiB 内存 / 1MiB 栈 / 5 秒执行时间；JS 返回值最大 32MiB
- `java.ajax`/`java.get`/`java.post`/`java.put`：30 秒超时，响应体上限 32MiB
- 出站地址受守卫约束：SECURE 模式下内网/环回地址被拦截（此时 `java.ajax` 静默返回空串，jsLib 远程条目求值失败）；本机单用户部署默认放行
- jsLib 的远程 URL 条目同样受出站守卫与 32MiB 上限约束
