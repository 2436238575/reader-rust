# CLAUDE.md

本文件是给 Claude Code 的**导入入口**，不重复维护内容。

**请先读 [`AGENTS.md`](./AGENTS.md)** —— 它是本仓库工程事实的唯一权威，包含：

- 项目定位与仓库/文档站地址
- 后端、前端、e2e、发布四组命令
- 完整配置项表（含代码默认值）
- 代码结构与请求链路
- 书源规则引擎（五种解析方式、URL 占位符、组合规则）
- 数据与存储、测试规模
- 开发约定（含「文档描述但未实现的功能要标注保留，不要删除」规则）
- 文档地图

## 三条最容易踩的坑

1. **必须在仓库根目录运行后端。** `storage/` 与 `.env` 都按相对路径解析，换目录会导致数据写到别处。
2. **前端目录是 `frontend/`（Vue 3 + Vite + TypeScript + Pinia）。** 仓库中没有 `web/` 目录，也没有 Vue 2 前端。可用脚本只有 `dev` / `build` / `preview` / `test`，没有 `serve` 和 `lint`。
3. **鉴权不是 JWT。** 是服务端自生成的不透明 token（形如 `用户名:token`），持久化在 SQLite 的 `users.token` 与 `user_sessions` 表，同一账号可多端登录。

## 改文档时

`README.md`、`AGENTS.md`、`docs/` 三者的读者不同，分工见 `AGENTS.md` 末尾的「文档地图」。改接口或配置时，请同步更新对应的 `docs/api/` 页面与 `docs/guide/configuration.md`。
