# 开发约定

## 环境准备

见 [快速开始](/guide/quickstart)。要点复述：

- 后端**必须在仓库根目录**运行 —— `storage/` 与 `.env` 按相对路径解析
- 前端目录是 `frontend/`（Vue 3 + Vite + TypeScript + Pinia），没有 `web/` 目录
- 首次 `cargo build` 需要 10 分钟以上（依赖里的 C 代码），之后增量构建几秒

## 分支与提交

- 主分支是 **`master`**，文档站只在 `master` / `main` 上触发部署
- 功能改动走分支 + PR。仓库里可见的协作者分支用 `codex/*` 前缀
- 提交信息用约定式前缀：`feat(scope): ...`、`fix(scope): ...`、`refactor(scope): ...`、`docs: ...`
- 发布提交由脚本生成，格式固定为 `release: vX.Y.Z`

## 提交前检查清单

```bash
cargo fmt --check                 # 格式
cargo clippy --all-targets        # 静态检查
cargo test                        # Rust 测试
cd frontend && npm run build      # 含 vue-tsc 类型检查
```

涉及界面或交互流程时，另外跑一次 e2e（需先启动后端）：

```bash
npm run test:e2e
```

::: warning 类型错误会中断构建
前端构建脚本是 `vue-tsc -b && vite build` —— 类型错误直接导致构建失败，不是警告。提交前务必跑一次 `npm run build`。
:::

## 测试要求

- **新增功能请补测试。** 集成测试放 `tests/`，纯逻辑单元测试就近放在 `src` 内的内联模块里；前端测试用 vitest（`*.test.ts` 与被测模块同目录）
- 当前规模：Rust 86 个用例（56 个 `#[test]` + 30 个 `#[tokio::test]`），前端 20 个 `*.test.ts`
- 改动解析器时，务必在 `tests/book_source_compat.rs` 里补上对应的书源格式用例 —— 这是兼容性的主要防线
- 注意 `tests/yckceo_live_sources.rs` 会**真的联网**抓在线书源，无网环境下失败属预期行为，不要为了「让测试全绿」而删它

完整测试流程见 [测试流程](/guide/testing)。

## 文档维护规则

### 单一来源

| 文档 | 维护者须知 |
|------|-----------|
| `AGENTS.md` | **工程事实的唯一权威**。配置默认值、命令、结构、测试规模以它为准 |
| `CLAUDE.md` | 只是导入入口，指向 `AGENTS.md`。**不要在这里重复维护内容** |
| `README.md` | 访客视角，只讲简介、特性与最快上手路径 |
| `docs/` | 面向用户的文档站，见 [文档地图](#文档地图) |

### 未实现的功能要标注，不要删除

**文档是开发目标的一部分。** 若某功能只在文档中描述、代码尚未实现，**保留条目并标注状态**：

```text
> **未实现**：<说明>（依据：<代码位置>）
```

这样后来者能区分「没做」和「做过后移除了」。历史设计与计划快照统一放 `docs/archive/`，加归档说明后保留，同样不删除。

### 改动要同步文档

| 改了什么 | 要同步更新 |
|---------|-----------|
| 新增 / 修改 `/reader3/*` 接口 | `docs/api/` 对应页面（路由以 `src/api/router.rs` 为准） |
| 新增 / 修改配置项 | `AGENTS.md` 的配置表 + `docs/guide/configuration.md`；注意 `app/config.rs` 里 `AppConfig`、`Default`、`set_default` 三处都要改 |
| 修改书源规则语法 | `docs/book-source/` 与 `docs/reference/book-source-rules.md` |
| 改动发布流程 | `docs/maintainers/release.md` |
| 改动前端可用脚本 | `AGENTS.md` 与 `docs/guide/quickstart.md` |

写文档时的硬要求：

- **不在文档里写本机绝对路径**，一律用仓库相对路径或占位符
- 接口路径、HTTP 方法必须与 `src/api/router.rs` 逐字一致（含 `bookSourceDebugSSE` 这类大写）
- 响应字段用**序列化后的名字**（本项目 `model/` 里大量 `#[serde(rename)]`）
- 示例代码块标注语言；中文文档用中文标点，代码与标识符保持原样

## 文档地图

```
README.md                      访客 / 使用者：简介、特性、最快上手
AGENTS.md                      AI 代理 / 贡献者：工程事实权威
docs/
  index.md                     文档站首页
  guide/                       部署、配置、功能、用户手册、测试流程
  api/                         /reader3/* 接口参考
  book-source/                 书源规则教程（面向书源作者）
  reference/                   兼容规格（面向实现者）
  maintainers/                 架构、发布、开发约定（本目录）
  archive/                     历史设计与计划快照
  images/                      文档配图
```

## 不要提交

- `storage/` —— 运行期数据（数据库、章节缓存、上传资源）
- `.env` —— 本地配置
- `target/`、`frontend/dist/`、`docs/.vitepress/dist`、`docs/.vitepress/cache` —— 构建产物
- `node_modules/`

## 版本号

版本号在**三处**保持一致，由 `scripts/release.sh` 自动同步：

- `Cargo.toml` 的 `[package] version`
- 根 `package.json` 的 `version`（e2e 测试工程的版本）
- `frontend/package.json` 的 `version`

手工改版本号会破坏一致性检查，请走发布脚本。

## 扩展点速查

| 想做什么 | 动哪里 |
|---------|--------|
| 加接口 | `api/router.rs` 注册 + `api/handlers/<领域>.rs` 写 handler，业务逻辑放 `service/` |
| 改解析行为 | `parser/rule_engine.rs`；组合规则切分在 `parser/rule_analyzer.rs` |
| 加 URL 占位符 | `crawler/url_analyzer.rs` |
| 加数据库表 | 在 `storage/db/migrations/` **新增**迁移文件，不要改历史迁移 |
| 加配置项 | `app/config.rs` 三处 + 两份配置文档 |
| 改前端接口封装 | `frontend/src/api/http.ts` 与对应模块 |

详见 [架构说明](./architecture)。
