# Docker Release Runbook

> 本文件是**导入入口**，完整内容已合并到文档站，请勿在此重复维护。

镜像构建与推送的完整说明（标签规则、`podman build --platform` 用法、架构校验、手动兜底命令、多架构 manifest）见：

- 文档站：<https://givenge.github.io/reader-rust/maintainers/release>
- 仓库内： [`docs/maintainers/release.md`](./docs/maintainers/release.md)

## 要点速记

- 镜像仓库默认 `docker.io/givenge/reader-rust`，可用 `DOCKER_REPO` 覆盖
- 版本化标签：`vX.Y.Z-x86_64`（amd64）、`vX.Y.Z-aarch64`（arm64）
- 滚动标签：`latest` → x86_64，`latest-aarch64` → arm64
- **Dockerfile 全链路多阶段构建**：源码进镜像出，不需要本机预编译（首次构建约 10 分钟以上，耗在后端 musl 编译）
- 构建时必须显式指定平台与 target：`--platform linux/amd64 --build-arg TARGET_TRIPLE=x86_64-unknown-linux-musl` / `--platform linux/arm64 --build-arg TARGET_TRIPLE=aarch64-unknown-linux-musl`

完整流程直接跑 `./scripts/release.sh`。
