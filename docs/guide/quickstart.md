# 快速开始

## 环境要求

| 组件 | 版本要求 | 用途 |
|------|---------|------|
| Rust | 最新稳定版（推荐通过 [rustup](https://rustup.rs/) 安装） | 编译后端 |
| Node.js | 20 及以上 | 构建 / 开发前端 |
| npm | 随 Node 附带 | 依赖管理 |
| Docker | 可选 | 容器化部署 |

Windows 下推荐使用 **MSVC 工具链**（`x86_64-pc-windows-msvc`），需要已安装 Visual Studio Build Tools 与 Windows SDK —— 因为依赖里的 `rquickjs`、`ring`、`libsqlite3-sys` 需要编译 C 代码。

## 获取源码

```bash
git clone https://github.com/givenge/reader-rust.git
cd reader-rust
```

## 运行后端

```bash
# 开发模式
cargo run

# 或构建发布版
cargo build --release
./target/release/reader-rust
```

::: warning 必须在仓库根目录运行
`storage/` 与 `.env` 都按**相对路径**解析。在别的目录启动会把数据写到意料之外的位置。
:::

首次构建需要 10 分钟以上（主要集中在依赖的 C 代码编译），之后增量构建只需数秒。首次启动会自动创建 `storage/` 目录、SQLite 数据库并执行迁移，无需手工初始化。

服务默认监听 `0.0.0.0:8080`。

## 构建并运行前端

前端有两种用法：

**方式一：构建为静态文件由后端托管（生产用法）**

```bash
cd frontend
npm install
npm run build      # 输出到 frontend/dist/
```

`frontend/dist/` 正是后端默认的 `WEB_ROOT`，构建完直接访问 `http://localhost:8080` 即可。

**方式二：Vite 开发服务器（改前端时用）**

```bash
cd frontend
npm run dev        # 默认 http://localhost:5173
```

开发服务器会把 `/reader3` 反向代理到后端（见 `frontend/vite.config.ts`）。若你改了后端端口，记得同步修改代理目标。

## 验证安装

```bash
curl http://127.0.0.1:8080/health
```

返回正常即表示后端已就绪。随后浏览器打开：

- 生产/后端托管模式：`http://localhost:8080`
- 开发模式：`http://localhost:5173`

能进入界面并成功添加一个书源，就说明前后端都跑通了。

## 默认配置

配置优先级：**环境变量 > `.env` 文件 > 代码默认值**。

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `SERVER_HOST` | `0.0.0.0` | 服务器绑定地址 |
| `SERVER_PORT` | `8080` | 服务器端口 |
| `DATABASE_URL` | `sqlite:storage/reader.db?mode=rwc` | SQLite 连接串 |
| `STORAGE_DIR` | `storage` | 运行期数据根目录 |
| `ASSETS_DIR` | `storage/assets` | 上传资源目录 |
| `WEB_ROOT` | `frontend/dist` | 前端静态文件目录 |
| `LOG_LEVEL` | `info` | 日志级别 |
| `REQUEST_TIMEOUT_SECS` | `15` | 抓取上游站点的超时时间 |

需要改配置时：

```bash
cp .env.example .env
# 编辑 .env
```

`SECURE`、`INVITE_CODE`、用户配额等其余配置项见 [配置](./configuration)。

## 常见问题

**端口被占用（Windows 上常见 `os error 10013`）**

Windows 存在系统保留端口段，绑定落在保留段内的端口会直接失败。换一个端口即可：

```bash
SERVER_PORT=18080 cargo run
```

**`cargo build` 编译失败，提示找不到 `link.exe` 或 C 编译器**

说明 MSVC 构建工具缺失。安装 Visual Studio Build Tools（勾选「使用 C++ 的桌面开发」）后重开终端。

**前端 `npm run build` 报类型错误**

构建脚本是 `vue-tsc -b && vite build`，类型错误会直接中断构建。按提示修完类型再构建。

## 下一步

- [配置](./configuration) —— 全部配置项
- [功能特性](./features) —— 已实现功能清单
- [Docker 部署](./docker)
- [书源开发](../book-source/) —— 写你自己的书源
