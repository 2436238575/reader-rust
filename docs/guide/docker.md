# Docker 部署

Reader-Rust 官方镜像发布在 Docker Hub 的 `docker.io/givenge/reader-rust`。

## 镜像标签

| 标签             | 架构          | 说明                          |
| ---------------- | ------------- | ----------------------------- |
| `latest`         | `linux/amd64` | x86_64 滚动标签，跟随最新发布 |
| `latest-aarch64` | `linux/arm64` | ARM64 滚动标签                |
| `vX.Y.Z-x86_64`  | `linux/amd64` | 具体版本                      |
| `vX.Y.Z-aarch64` | `linux/arm64` | 具体版本                      |

## 直接运行

**x86_64：**

```bash
docker pull givenge/reader-rust:latest

docker run -d \
  --name reader \
  -p 8080:8080 \
  -v $(pwd)/storage:/app/storage \
  --restart unless-stopped \
  givenge/reader-rust:latest
```

**ARM64：**

```bash
docker pull givenge/reader-rust:latest-aarch64

docker run -d \
  --name reader \
  -p 8080:8080 \
  -v $(pwd)/storage:/app/storage \
  --restart unless-stopped \
  givenge/reader-rust:latest-aarch64
```

启动后访问 `http://localhost:8080`。

## 镜像内置的环境变量

镜像里已经设好了这些值（见 `Dockerfile`），通常不用改：

| 变量           | 镜像内取值                               |
| -------------- | ---------------------------------------- |
| `SERVER_HOST`  | `0.0.0.0`                                |
| `SERVER_PORT`  | `8080`                                   |
| `DATABASE_URL` | `sqlite:/app/storage/reader.db?mode=rwc` |
| `STORAGE_DIR`  | `/app/storage`                           |
| `ASSETS_DIR`   | `/app/storage/assets`                    |
| `WEB_ROOT`     | `/app/web/dist`                          |
| `LOG_LEVEL`    | `info`                                   |

因此**只需挂载 `/app/storage` 一个卷**，数据库、章节缓存、上传资源就都在里面了。

## 数据持久化

```bash
-v $(pwd)/storage:/app/storage
```

该目录包含：

- SQLite 数据库（`reader.db`）
- 章节正文缓存（`cache/`）
- 上传的资源与本地书籍（`assets/`）

镜像声明了 `VOLUME ["/app/storage"]`。若不显式挂载，容器重建后数据会丢失。

## 自定义配置

需要覆盖默认值时通过 `-e` 传入，例如开启安全模式：

```bash
docker run -d \
  --name reader \
  -p 8080:8080 \
  -v $(pwd)/storage:/app/storage \
  -e JWT_SECRET=your-random-secret \
  -e ADMIN_PASSWORD=your-strong-password \
  -e LOG_LEVEL=debug \
  givenge/reader-rust:latest
```

全部配置项见 [配置](./configuration)。

## Docker Compose

仓库未附带 `docker-compose.yml`，需要的话自行创建：

```yaml
services:
  reader:
    image: givenge/reader-rust:latest
    container_name: reader
    ports:
      - '8080:8080'
    volumes:
      - ./storage:/app/storage
    environment:
      - JWT_SECRET=your-random-secret
      - LOG_LEVEL=info
    restart: unless-stopped
```

```bash
docker compose up -d
```

## 自行构建镜像

::: warning 构建耗时
`Dockerfile` 是**全链路多阶段构建**：前端 node 阶段 `npm ci + build`，后端 muslrust 阶段 `cargo build --release`，运行时 alpine 只装产物——源码进、镜像出，可复现，不依赖本机预编译。首次构建约 10 分钟以上（耗在后端 musl 编译），后续命中层缓存很快。

```bash
# 构建镜像（必须显式指定 platform；TARGET_TRIPLE 与之对应）
podman build --platform linux/amd64 --build-arg TARGET_TRIPLE=x86_64-unknown-linux-musl -t yourname/reader-rust:latest .
podman build --platform linux/arm64 --build-arg TARGET_TRIPLE=aarch64-unknown-linux-musl -t yourname/reader-rust:latest-aarch64 .
```

交叉编译的 linker 配置在仓库根目录的 `.cargo/config.toml` 中。
:::

## 发布流程

打版本、推镜像、建 GitHub Release 的完整步骤见 [`RELEASE_WORKFLOW.md`](https://github.com/givenge/reader-rust/blob/master/RELEASE_WORKFLOW.md)，一条命令搞定：

```bash
./scripts/release.sh
```
