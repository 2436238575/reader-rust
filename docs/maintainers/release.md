# 发布流程

发布统一走 **`scripts/release.sh`** 一条命令，它串起版本号同步、前后端构建、交叉编译、Docker 镜像构建与推送、git tag、GitHub Release。

::: danger 不要手工打 tag 或手工推镜像
脚本会校验 tag 与 GitHub Release 是否已存在，手工操作容易造成版本号不同步（`Cargo.toml` / 根 `package.json` / `frontend/package.json` 三者）或镜像标签错乱。
:::

## 前置条件

脚本开头会逐个校验，缺任何一个直接退出：

| 命令     | 用途                                                                    |
| -------- | ----------------------------------------------------------------------- |
| `git`    | 提交与打 tag                                                            |
| `cargo`  | 交叉编译后端                                                            |
| `npm`    | 同步版本号、构建前端（`node` 随 npm 提供，无 tag 时的版本回退也用到它） |
| `podman` | 构建与推送镜像                                                          |
| `gh`     | 创建 GitHub Release                                                     |
| `awk`    | 改写 `Cargo.toml` 里的版本号                                            |

另外：

- **`gh` 必须已登录**：`gh auth login`
- **`podman` 必须已登录 Docker Hub**：`podman login docker.io`
- **工作区必须完全干净**，包括**没有未跟踪文件**（脚本会跑 `git ls-files --others --exclude-standard` 检查）。注意 `.env`、`storage/`、`docs/.vitepress/dist` 等都应在 `.gitignore` 里，否则会挡住发布
- 两个 musl 目标需要已安装：`rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl`
- `.cargo/config.toml` 里已配好交叉编译的 linker

## 用法

从仓库根目录执行：

```bash
./scripts/release.sh                # 在最新 tag 上自动递增 patch
./scripts/release.sh --minor        # 递增次版本号
./scripts/release.sh --major        # 递增主版本号
./scripts/release.sh --patch        # 明确指定 patch（默认即此）
./scripts/release.sh v1.0.9         # 发布指定版本（也接受 1.0.9）
./scripts/release.sh --help
```

版本号推导规则：

1. 显式给了版本号 → 用它（前缀 `v` 可有可无）
2. 否则取 `git tag --list 'v*'` 中版本排序最大的那个作为基准，按 `--major` / `--minor` / `--patch` 递增
3. 若一个 `v*` tag 都没有，退回读 `frontend/package.json` 的 `version` 作为基准

## 脚本做了什么

按实际执行顺序：

1. **校验环境** —— 依赖命令、工作区干净、无未跟踪文件、`gh` 已登录
2. **解析目标版本** —— 按上面的规则得出 `SEMVER` 与 `TAG`（`v` 前缀）
3. **冲突检查** —— 本地已存在同名 tag，或 GitHub 上已存在同名 Release，则退出
4. **同步版本号** —— 三处一起改：
   - `Cargo.toml` 的 `[package] version`（用 `awk` 只改 `[package]` 段内的第一处）
   - 根 `package.json`（`npm version --no-git-tag-version`）
   - `frontend/package.json`（同上）
5. **构建前端** —— `cd frontend && npm install && npm run build`，产出 `frontend/dist/`
6. **交叉编译后端** —— 依次构建 `x86_64-unknown-linux-musl` 与 `aarch64-unknown-linux-musl`
7. **提交并打 tag** —— `git add` 六个版本文件（含两个 lock 文件）；若暂存区为空（版本号本来就一致），跳过提交、直接给当前提交打 tag。tag 是带注释的：`git tag -a vX.Y.Z`
8. **推送** —— 推送当前分支与 tag 到 `origin`
9. **构建镜像** —— 显式指定平台：
   - `--platform linux/amd64 -f Dockerfile.x86` → `docker.io/givenge/reader-rust:vX.Y.Z-x86_64`
   - `--platform linux/arm64 -f Dockerfile` → `docker.io/givenge/reader-rust:vX.Y.Z-aarch64`
10. **校验镜像架构** —— `podman image inspect` 结果必须是 `amd64 linux` 与 `arm64 linux`，否则**退出**（此时版本已推、镜像未推，需人工处理）
11. **推送版本化镜像标签**
12. **更新并推送滚动标签** —— `latest` ← x86_64，`latest-aarch64` ← arm64
13. **创建 GitHub Release** —— `gh release create --generate-notes`

成功后会打印 GitHub Release 地址与四个镜像标签。

## 镜像标签规则

| 标签             | 架构          | 类型               |
| ---------------- | ------------- | ------------------ |
| `latest`         | `linux/amd64` | 滚动，跟随最新发布 |
| `latest-aarch64` | `linux/arm64` | 滚动               |
| `vX.Y.Z-x86_64`  | `linux/amd64` | 版本化，不可变     |
| `vX.Y.Z-aarch64` | `linux/arm64` | 版本化，不可变     |

仓库默认是 `docker.io/givenge/reader-rust`，可用环境变量覆盖（例如发布到自己的 fork）：

```bash
DOCKER_REPO=docker.io/yourname/reader-rust ./scripts/release.sh
```

::: warning 文档站不在发布脚本内
`docs/` 由独立的 GitHub Actions 工作流 `.github/workflows/docs.yml` 部署 —— 推送到 `master` / `main` 且改动涉及 `docs/**` 时自动构建并发布到 GitHub Pages。因此**改文档不需要跑 release.sh**。
:::

## 手动兜底

正常情况下永远不需要手动执行。若脚本在第 10 步之后失败（镜像已构建但推送中断），可按下面的命令补齐，**不要重新跑 `release.sh`**（tag 已存在，会被冲突检查挡住）：

```bash
export TAG=v1.0.9

# 校验镜像架构
podman image inspect docker.io/givenge/reader-rust:${TAG}-x86_64 --format '{{.Architecture}} {{.Os}}'
podman image inspect docker.io/givenge/reader-rust:${TAG}-aarch64 --format '{{.Architecture}} {{.Os}}'
# 期望：amd64 linux / arm64 linux

# 推送版本化标签
podman push docker.io/givenge/reader-rust:${TAG}-x86_64
podman push docker.io/givenge/reader-rust:${TAG}-aarch64

# 更新滚动标签
podman tag docker.io/givenge/reader-rust:${TAG}-x86_64 docker.io/givenge/reader-rust:latest
podman tag docker.io/givenge/reader-rust:${TAG}-aarch64 docker.io/givenge/reader-rust:latest-aarch64
podman push docker.io/givenge/reader-rust:latest
podman push docker.io/givenge/reader-rust:latest-aarch64

# 补建 Release
gh release create "$TAG" --title "$TAG" --generate-notes
```

若还需要一个**多架构统一标签**（`docker.io/givenge/reader-rust:${TAG}` 同时指向 amd64 与 arm64）：

```bash
podman manifest create docker.io/givenge/reader-rust:${TAG}
podman manifest add docker.io/givenge/reader-rust:${TAG} docker.io/givenge/reader-rust:${TAG}-x86_64
podman manifest add docker.io/givenge/reader-rust:${TAG} docker.io/givenge/reader-rust:${TAG}-aarch64
podman manifest push --all docker.io/givenge/reader-rust:${TAG}
```

## 常见失败

| 现象                                | 原因                                                                              |
| ----------------------------------- | --------------------------------------------------------------------------------- |
| `Working tree is not clean`         | 有未提交改动 —— 先 commit 或 stash                                                |
| `Untracked files exist`             | 有未跟踪文件（如临时产物）—— 清理或加进 `.gitignore`                              |
| `GitHub CLI is not authenticated`   | 没跑 `gh auth login`                                                              |
| `Missing required command: podman`  | Podman 未安装或不在 PATH                                                          |
| `Tag vX.Y.Z already exists locally` | 重复发布同一版本；要重发必须先删本地 tag，且确认远端也要处理                      |
| 镜像架构校验失败                    | 构建时 `--platform` 与 Dockerfile 不匹配；检查 `podman build` 是否用了正确的 `-f` |

`Dockerfile` 与 `Dockerfile.x86` **都不编译 Rust**，只负责把宿主机编译好的二进制与 `frontend/dist` 拷进镜像 —— 所以脚本必须先在宿主机完成第 5、6 步。
