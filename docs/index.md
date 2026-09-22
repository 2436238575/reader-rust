---
layout: home

hero:
  name: "Reader-Rust"
  text: "阅读服务端 · Rust 版"
  tagline: 高性能书源阅读服务器，支持自定义书源与五种规则解析方式
  actions:
    - theme: brand
      text: 快速开始
      link: /guide/quickstart
    - theme: alt
      text: 用户手册
      link: /guide/user-manual
    - theme: alt
      text: API 参考
      link: /api/

features:
  - icon: ⚡
    title: 高性能
    details: 基于 Rust 与 axum 构建，内存安全，多书源并发抓取
  - icon: 🔧
    title: 灵活书源
    details: 支持 CSS 选择器、JSONPath、XPath、正则、JavaScript 五种解析方式
  - icon: 🌐
    title: 规则引擎
    details: 自动识别内容类型并匹配解析规则，完整兼容阅读3.0 书源语法
  - icon: 💾
    title: 数据持久化
    details: SQLite 存储书源与用户数据，章节正文以文件形式缓存
  - icon: 👥
    title: 多用户
    details: 账号体系、数据按命名空间隔离、书架与本地书籍配额
  - icon: 📱
    title: 配套 Web 界面
    details: Vue 3 + TypeScript，支持亮暗主题、简繁转换、TTS 与 PWA 离线阅读
---

## 从这里开始

| 我想… | 去哪 |
|-------|------|
| 把服务跑起来 | [快速开始](/guide/quickstart) · [Docker 部署](/guide/docker) · [手动部署](/guide/manual-deploy) |
| 搞清楚配置项 | [配置](/guide/configuration) |
| 学着用界面 | [用户手册](/guide/user-manual) · [AI 资料](/guide/ai-book) |
| 对接 `/reader3/*` 接口 | [API 参考](/api/) |
| 自己写书源 | [书源开发](/book-source/) · [书源规则兼容规格](/reference/book-source-rules) |
| 改这个项目的代码 | 仓库根目录的 `AGENTS.md` · [架构说明](/maintainers/architecture) · [开发约定](/maintainers/development) |
| 发一个版本 | [发布流程](/maintainers/release) |

## 它是什么

Reader-Rust 是 [阅读3.0](https://github.com/hectorqin/reader) 的 Rust 重写版 —— 一个**书源阅读 API 服务端**，附带一套 Vue 3 Web 界面。它负责管理书源、抓取上游站点、按书源规则解析出书籍与章节内容，并维护书架与用户数据。

::: warning 免责声明
本项目只提供书源管理、内容解析、阅读与缓存的技术能力，**不内置、不存储、不分发**任何受版权保护的书籍内容。用户应确保自行添加的书源、上传的本地文件以及通过本服务访问的内容均已获得合法授权。
:::

## 相关链接

- [GitHub 仓库](https://github.com/givenge/reader-rust)
- [ai-source-designer](https://github.com/givenge/ai-source-designer) —— agent 驱动的书源设计工具
- [原项目 reader](https://github.com/hectorqin/reader)
