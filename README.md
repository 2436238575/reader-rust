# Reader-Rust

基于 [reader](https://github.com/hectorqin/reader) 重构的 Rust 版「阅读3.0」书源阅读服务端。

- 仓库：<https://github.com/givenge/reader-rust>
- 完整文档：<https://givenge.github.io/reader-rust/>
- 作者：[@grandy](https://linux.do/u/grandy/summary)

## 免责声明

本项目仅提供书源管理、内容解析、阅读与缓存等技术能力，**不内置、不存储、不分发**任何受版权保护的书籍内容。用户应确保自行添加的书源、上传的本地文件以及通过本服务访问的内容均已获得合法授权，并自行承担由此产生的版权与合规责任。

如任何权利人认为本项目相关内容或使用方式侵犯了其合法权益，请通过项目 Issues 联系维护者，我们将在核实后及时处理。

## 功能特性

- **自定义书源**：导入、编辑、分组、启用/禁用、远程订阅同步，支持批量导入导出
- **五种解析方式**：CSS 选择器、JSONPath、XPath、正则、JavaScript，规则引擎自动识别内容类型
- **搜索与发现**：单源搜索、多源并发搜索（SSE 实时返回）、按分类浏览书源书籍
- **阅读与缓存**：目录拉取、章节正文解析、章节缓存预下载（SSE 实时进度）、书签、阅读进度
- **本地书籍**：支持上传 TXT 与 EPUB
- **AI 资料**：用 OpenAI 兼容模型整理已读章节的世界观、角色、关系与地点地图
- **单用户账号**：首次启动自动建号，初始密码打印到日志，改密即吊销其他设备的登录态
- **配套 Web 界面**：Vue 3 + TypeScript，支持亮/暗主题、简繁转换、TTS 朗读与 PWA 离线阅读

## 技术栈

| 层 | 技术 |
|----|------|
| 后端 | Rust + axum + tokio + reqwest + sqlx (SQLite) + rquickjs |
| 前端 | Vue 3 + TypeScript + Vite + Pinia |
| 文档站 | VitePress |

## 快速开始

### 使用 Docker（推荐）

```bash
# x86_64
docker pull givenge/reader-rust:latest

# ARM64
docker pull givenge/reader-rust:latest-aarch64

docker run -d \
  --name reader \
  -p 8080:8080 \
  -v $(pwd)/storage:/app/storage \
  givenge/reader-rust:latest
```

镜像内已设好 `WEB_ROOT=/app/web/dist`、`STORAGE_DIR=/app/storage`，只需挂载 `/app/storage` 即可持久化数据。访问 `http://localhost:8080` 打开界面。

### 从源码运行

```bash
git clone https://github.com/givenge/reader-rust.git
cd reader-rust

# 后端（必须在仓库根目录运行，storage/ 与 .env 按相对路径解析）
cargo run
```

后端默认监听 `0.0.0.0:8080`。前端若要单独起开发服务器：

```bash
cd frontend
npm install
npm run build      # 构建产物输出到 frontend/dist/，由后端直接托管
npm run dev        # 或启动 Vite 开发服务器（默认 http://localhost:5173）
```

开发服务器会把 `/reader3` 反向代理到后端，代理目标端口需与后端 `SERVER_PORT` 一致。

### 验证服务

```bash
curl http://127.0.0.1:8080/health
```

## 配置

配置优先级：**环境变量 > `.env` 文件 > 代码默认值**。

```bash
cp .env.example .env
```

常用项：

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `SERVER_HOST` | `0.0.0.0` | 监听地址 |
| `SERVER_PORT` | `8080` | 监听端口 |
| `DATABASE_URL` | `sqlite:storage/reader.db?mode=rwc` | SQLite 连接串 |
| `WEB_ROOT` | `frontend/dist` | 前端静态文件目录 |
| `LOG_LEVEL` | `info` | 日志级别 |
| `JWT_SECRET` | 空 | JWT 签名密钥，留空时自动生成到 `storage/jwt_secret` |

完整配置项与说明见 [配置文档](https://givenge.github.io/reader-rust/guide/configuration)。

## 开发

```bash
cargo test                     # Rust 测试
cd frontend && npm test        # 前端单元测试
npm install && npm run test:e2e   # Playwright 端到端测试（需先启动后端）
```

工程约定、代码结构、接口清单与发布流程见 [`AGENTS.md`](./AGENTS.md)。

## 发布

```bash
./scripts/release.sh           # 自动递增 patch 并发布前后端与镜像
```

详见 [`RELEASE_WORKFLOW.md`](./RELEASE_WORKFLOW.md)。

## 相关工具

- [ai-source-designer](https://github.com/givenge/ai-source-designer) —— agent 驱动的书源设计工具

## 许可证

仓库当前未附带开源许可证文件，如需商用或二次分发请先联系维护者确认授权方式。
