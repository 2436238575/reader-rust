# 手动部署

适用于不用 Docker、直接跑二进制的场景（裸机或 VPS）。

## 1. 准备构建环境

- Rust 最新稳定版（[rustup](https://rustup.rs/)）
- Node.js 20 及以上
- Linux 目标若需静态链接，另需 `musl-tools`（Debian/Ubuntu：`apt install musl-tools`）

## 2. 构建产物

```bash
git clone https://github.com/givenge/reader-rust.git
cd reader-rust

# 前端
cd frontend && npm install && npm run build && cd ..
# 部署在非根路径（如 https://example.org/read/）时加部署前缀：
# VITE_BASE_PATH=/read/ npm run build

# 后端（本机架构）
cargo build --release
# 产物：target/release/reader-rust
```

### 交叉编译静态二进制

静态链接的二进制不依赖目标机的 glibc，便于分发：

```bash
# x86_64
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl

# aarch64
rustup target add aarch64-unknown-linux-musl
cargo build --release --target aarch64-unknown-linux-musl
```

产物分别位于 `target/<target-triple>/release/reader-rust`。linker 配置在仓库根目录的 `.cargo/config.toml` 中。

## 3. 部署目录

```bash
mkdir -p /opt/reader/storage/assets /opt/reader/web
cp target/release/reader-rust /opt/reader/
cp -r frontend/dist/. /opt/reader/web/dist/
```

目录规划：

| 路径                          | 内容                           |
| ----------------------------- | ------------------------------ |
| `/opt/reader/reader-rust`     | 可执行文件                     |
| `/opt/reader/web/dist/`       | 前端静态文件                   |
| `/opt/reader/storage/`        | 运行期数据（数据库、章节缓存） |
| `/opt/reader/storage/assets/` | 上传资源（本地书保存在 `storage/data/`） |

## 4. 创建 `.env`

在 `/opt/reader/.env`：

```ini
SERVER_HOST=0.0.0.0
SERVER_PORT=8080
DATABASE_URL=sqlite:storage/reader.db?mode=rwc
STORAGE_DIR=storage
ASSETS_DIR=storage/assets
WEB_ROOT=web/dist
LOG_LEVEL=info

JWT_SECRET=<足够随机的密钥>
ADMIN_PASSWORD=<登录密码>
```

::: warning 路径都相对于工作目录
`DATABASE_URL`、`STORAGE_DIR`、`WEB_ROOT` 均按**相对路径**解析，基准是进程的工作目录。所以务必让 systemd 的 `WorkingDirectory` 指向 `/opt/reader`，与上面的配置保持一致。
:::

代码默认的 `WEB_ROOT` 是 `frontend/dist`，上表改成 `web/dist` 是为了匹配本页的目录布局；不做这层调整就保持默认值也可。

## 5. systemd 服务

创建 `/etc/systemd/system/reader.service`：

```ini
[Unit]
Description=Reader-Rust
After=network.target

[Service]
Type=simple
User=www-data
Group=www-data
WorkingDirectory=/opt/reader
ExecStart=/opt/reader/reader-rust
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
```

```bash
sudo chown -R www-data:www-data /opt/reader
sudo systemctl daemon-reload
sudo systemctl enable --now reader
sudo systemctl status reader
```

验证：

```bash
curl http://127.0.0.1:8080/health
```

## 6. Nginx 反向代理

```nginx
server {
    listen 80;
    server_name your-domain.com;

    # 大文件上传（本地书籍、备份导入）
    client_max_body_size 100m;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # SSE 接口（搜索、缓存、书源调试）需要关闭缓冲
        proxy_buffering off;
        proxy_read_timeout 300s;
    }
}
```

::: tip SSE 与长连接
搜索、缓存整本书、书源调试等接口使用 SSE 流式返回，`proxy_buffering off` 是必需的，否则前端会一直收不到数据。同时建议放开 `proxy_read_timeout`。
:::

### 部署到子路径

想把整个服务挂在 `https://example.org/read/` 这样的子路径下，分三步：

**1. 带前缀构建前端**（不加前缀时产物里的资源与接口地址都指向 `/`）：

```bash
cd frontend
VITE_BASE_PATH=/read/ npm run build
```

**2. nginx 里剥掉前缀再转发**（`proxy_pass` 末尾的 `/` 是关键，
它会把 `/read/` 前缀去掉，后端仍按根路径处理）：

```nginx
location /read/ {
    proxy_pass http://127.0.0.1:8080/;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;

    # 大文件上传（本地书籍、整本 TXT 导入、备份导入）；
    # nginx 默认只有 1m，整本 TXT 会被 413 或截断
    client_max_body_size 100m;

    proxy_buffering off;
    proxy_read_timeout 300s;
}
```

**3. 让后端把会话 Cookie 指到带前缀的路径**：

```bash
# .env 或容器环境变量
COOKIE_PATH=/read/reader3
```

反代剥掉前缀后，后端看到的请求仍是 `/v1/*`；`COOKIE_PATH` 用来推导
refresh Cookie 与书源代理页的前缀（`/read/reader3` → `/read/v1/auth`、
`/read/v1/sources/...`）。不设时浏览器实际地址带 `/read/` 前缀而 Cookie
路径不带，refresh/代理请求就不会回传 Cookie。

要点：

- 前端是 hash 路由（`https://example.org/read/#/reader`），不需要服务端 SPA 回落。
- 服务端返回的图片路径（`/v1/images/<id>`）由前端统一套上部署前缀，
  后端不需要感知静态资源与图片路径；但**会话 Cookie 的 `Path` 必须显式配置**
  （见上面第 3 步）。
- PWA 的 Service Worker 与 manifest 都按自身位置推导作用域，
  子路径下装到桌面也不会互相干扰。
- 本地想先试：`VITE_BASE_PATH=/read/ npm run build && VITE_BASE_PATH=/read/ npm run preview`
  （`vite preview` 的代理同样会剥掉前缀，等价于上面的 nginx）。

::: warning 上传体积上限
整本 TXT 导入、本地书籍上传与备份导入的请求体可达几十 MB。除了 nginx /
OpenResty 的 `client_max_body_size`，链路上每一层都要放行：

- Cloudflare 免费/Pro 计划的请求体上限是 **100MB**，Enterprise 可调大；
- 雷池 SafeLine 等 WAF 如果配置了上传大小限制，也会在到达后端前截断或拒绝；
- 后端自身的大上传路由上限是 100MB，整本 TXT 另有 50MB 上限。

正文历史的整本 TXT 导入在文件超过 512KB 时会自动分片（每片从 512KB 起步、
失败减半），单个请求很小，不再受这里的上限影响；本地书籍上传与备份导入仍是
单请求，仍要放行上面的体积。后端会核对每份文件的实际字节数，不完整时返回明确的
「TXT 上传不完整」，不会把半本书静默写入正文历史。
:::

## 7. 升级

```bash
cd /path/to/reader-rust
git pull
cd frontend && npm install && npm run build && cd ..
cargo build --release

sudo systemctl stop reader
sudo cp target/release/reader-rust /opt/reader/
sudo cp -r frontend/dist/. /opt/reader/web/dist/
sudo systemctl start reader
```

`storage/` 目录不会被覆盖，数据与书源配置在升级后保持原样。数据库迁移由程序在启动时自动执行。

## 下一步

- [配置](./configuration) —— 全部配置项
- [Docker 部署](./docker) —— 更省事的替代方案
