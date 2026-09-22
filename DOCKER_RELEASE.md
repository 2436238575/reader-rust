# Docker Release Runbook

> 本文件是**导入入口**，完整内容已合并到文档站，请勿在此重复维护。

镜像构建与推送的完整说明（标签规则、`podman build --platform` 用法、架构校验、手动兜底命令、多架构 manifest）见：

- 文档站：<https://givenge.github.io/reader-rust/maintainers/release>
- 仓库内： [`docs/maintainers/release.md`](./docs/maintainers/release.md)

## 要点速记

- 镜像仓库默认 `docker.io/givenge/reader-rust`，可用 `DOCKER_REPO` 覆盖
- 版本化标签：`vX.Y.Z-x86_64`（amd64）、`vX.Y.Z-aarch64`（arm64）
- 滚动标签：`latest` → x86_64，`latest-aarch64` → arm64
- **两个 Dockerfile 都不编译 Rust**，必须先在本机交叉编译出二进制、并构建好 `frontend/dist`
- 构建时必须显式指定平台：`--platform linux/amd64 -f Dockerfile.x86` / `--platform linux/arm64 -f Dockerfile`

完整流程直接跑 `./scripts/release.sh`。
