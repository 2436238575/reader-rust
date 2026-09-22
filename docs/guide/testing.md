# 测试流程

本文定义 Reader-Rust 的标准回归流程，目标是让后端、前端与交互链路在发布前都有统一检查。

## 测试分层总览

| 层级 | 命令 | 覆盖范围 |
|------|------|---------|
| Rust 单元 + 集成测试 | `cargo test` | 86 个用例（见下） |
| 前端单元测试 | `cd frontend && npm test` | 20 个 `*.test.ts`（vitest） |
| 端到端冒烟 | `npm run test:e2e` | Playwright，需先启动后端 |
| 手工全量回归 | 见文末清单 | 依赖真实书源、账号、缓存的功能 |

## 1. Rust 测试

```bash
cargo test                     # 全部
cargo test book_source         # 按名称过滤
cargo test --lib               # 只跑 src 内的内联单元测试
```

Rust 侧共 **86 个用例**（56 个 `#[test]` + 30 个 `#[tokio::test]`），分两类：

- **`tests/` 下的集成测试**（10 个文件）：`book_source_compat.rs`（用例最多，覆盖书源格式兼容）、`book_source_validation.rs`、`book_source_headers.rs`、`local_txt_book.rs`、`local_epub_book.rs`、`ai_book_memory.rs`、`ai_proxy.rs`、`version_update.rs`、`js_compat.rs`、`yckceo_live_sources.rs`
- **`src/` 内的内联单元测试**：解析器、URL 处理等纯逻辑

::: warning 有一个用例会真的联网
`tests/yckceo_live_sources.rs` 会抓取在线书源列表，在无网络或受限环境下失败属于预期行为，不代表代码有问题。
:::

## 2. 前端单元测试

```bash
cd frontend
npm install
npm test                       # vitest run
```

覆盖 Pinia store 与工具函数，共 20 个 `.test.ts`。

## 3. 构建与静态检查

发布前至少执行：

```bash
cargo check
cargo test
cd frontend && npm run build    # 含 vue-tsc 类型检查
```

这一层负责发现：Rust 编译错误、解析器回归、前端类型错误、前端打包错误。

## 4. Playwright 端到端测试

配置与用例都在**仓库根目录**：

- `playwright.config.ts`
- `tests/e2e/*.spec.ts`

安装：

```bash
npm install
npx playwright install chrome
```

配置使用系统已安装的 Chrome（`channel: 'chrome'`），因此机器上需要有 Chrome。

执行：

```bash
# 先手动启动后端（另开一个终端）
cargo run

# 再跑 e2e
npm run test:e2e            # 无头
npm run test:e2e:headed     # 有头
npm run test:e2e:ui         # UI 模式
```

默认访问 `http://127.0.0.1:8080`。后端换了端口时用环境变量覆盖：

```bash
PLAYWRIGHT_BASE_URL=http://127.0.0.1:18080 npm run test:e2e
```

### 当前自动冒烟覆盖范围

- 顶部 / 底部导航切换
- 书架、书海、最近、RSS 主页面可打开
- 设置抽屉可打开
- 书源管理、用户管理、备份恢复入口可见
- 全局搜索可进入搜索模式
- 搜索结果范围切换 UI 可见
- 分组管理、缓存管理弹窗可打开
- RSS 管理页跳转可用

### 可选：需要测试数据的用例

部分用例通过环境变量提供数据，未设置时会跳过。

**登录回归**

```bash
E2E_USERNAME=admin E2E_PASSWORD=12345678 npm run test:e2e
```

**搜索结果详情回归**

```bash
E2E_SEARCH_KEYWORD=凡人修仙传 E2E_EXPECTED_BOOK_NAME=凡人修仙传 npm run test:e2e
```

**书源导入回归**

```bash
E2E_SOURCE_IMPORT_FILE=/absolute/path/to/book-sources.json npm run test:e2e
```

## 5. 标准手工回归清单

以下功能依赖真实书源、账号、缓存或设备行为，不适合全部硬编码进自动测试，发布前建议人工过一遍。

### A. 书架

1. 打开首页，确认书架列表正常显示
2. 打开一本已有书籍，确认能进入阅读页
3. 返回书架，确认最近阅读顺序更新
4. 打开书籍详情弹窗，确认目录能加载
5. 测试编辑模式、批量选择、分组移动、删除

### B. 搜索

1. 输入关键词进入搜索模式
2. 验证「全部书源 / 按分组 / 单个书源」都能切换
3. 验证搜索结果里显示书源信息和最新章节
4. 点击封面打开书籍详情
5. 点击结果项进入阅读
6. 点击「加入书架」后书架中可见

### C. 书源管理

1. 打开设置中的书源管理
2. 本地导入书源 JSON
3. 远程订阅同步一次
4. 启用 / 禁用书源
5. 编辑并保存一个书源
6. 触发书源登录，确认状态返回正常

### D. 阅读器

1. 打开章节目录并切换章节
2. 打开章节搜索面板并搜索当前章节
3. 修改字体、间距、主题等阅读设置
4. 测试正文分页或滚动模式是否正常
5. 验证 TTS 播放、暂停、续播、下一章
6. iOS / iPadOS 桌面模式验证首开和前后台切换

### E. 发现页

1. 切换不同发现书源
2. 切换不同分类
3. 滚动加载更多
4. 点击书籍进入阅读
5. 点击「加入书架」验证写入成功

### F. 最近阅读

1. 验证最近阅读列表显示正常
2. 搜索最近阅读
3. 打开历史条目继续阅读
4. 删除最近阅读条目

### G. RSS

1. 进入 RSS 首页
2. 切换当前源、全部文章、分组文章
3. 打开文章正文
4. 进入 RSS 管理页，新增 / 编辑 / 删除 RSS 源
5. 刷新文章并确认分页加载

### H. 用户与备份

1. 未登录时打开设置，确认登录入口正常
2. 登录后确认用户名和角色显示正常
3. 修改密码
4. 多用户模式下打开用户管理
5. 开启 WebDAV / 服务器备份时验证备份恢复入口

## 6. 发布前建议流程

1. `cargo check`
2. `cargo test`
3. `cd frontend && npm run build`
4. `npm run test:e2e`
5. 按「标准手工回归清单」执行一次完整人工检查

若本次改动涉及以下模块，需要追加专项回归：

| 改动模块 | 追加回归 |
|---------|---------|
| 解析器 | 书源导入、搜索、详情、目录、正文 |
| 阅读器 | 阅读页、TTS、移动端与 iOS/iPadOS |
| 用户系统 | 登录、登出、用户管理、权限 |
| RSS | RSS 管理与正文加载 |

## 后续可补充的方向

- 为 Playwright 增加专用 fixture 数据
- 增加测试账号与测试书源的初始化脚本
- 在 CI 中加入 e2e 冒烟任务
- 增加移动端视口与 PWA 模式专项测试
