# Release Workflow

> 本文件是**导入入口**，完整内容已统一到文档站，请勿在此重复维护。

发布流程（版本号规则、脚本逐步行为、镜像标签、失败处理、手动兜底命令）见：

- 文档站：<https://givenge.github.io/reader-rust/maintainers/release>
- 仓库内： [`docs/maintainers/release.md`](./docs/maintainers/release.md)

## 一句话版本

```bash
./scripts/release.sh           # 自动在最新 tag 上递增 patch 并发布
./scripts/release.sh v1.0.9    # 发布指定版本
```

前置条件：工作区**完全干净（含无未跟踪文件）**、`gh auth login` 完成、`podman login docker.io` 完成、两个 musl 交叉编译目标已安装。

不要在文档里手工打 tag 或手工推镜像 —— 脚本是唯一入口。

## 其它入口

- 工程事实权威：[`AGENTS.md`](./AGENTS.md)
- 面向使用者的部署说明：[`docs/guide/docker.md`](./docs/guide/docker.md)、[`docs/guide/manual-deploy.md`](./docs/guide/manual-deploy.md)
