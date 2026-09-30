# agent sidecar

AI 资料编排的 Python sidecar：agent 循环、提示词、patch 归一化合并、模型调用都在这里；
Rust 后端（`src/service/agent_sidecar_service.rs`）按任务 spawn 本进程，通过 stdin/stdout
的行式 JSON（NDJSON）通信。协议与运行方式见 `AGENTS.md` 与
`docs/guide/configuration.md`。

## 运行

```bash
cd sidecar
uv sync          # 推荐：自动建 .venv 并按 pyproject 钉版安装（uv.lock 已提交）
uv run pytest    # 全部测试
```

无 uv 时手动建 venv：`python -m venv .venv` 后
`.venv/Scripts/python -m pip install -e .[dev]`（POSIX 为 `.venv/bin/...`）。

- 依赖版本在 `pyproject.toml` 钉死（Docker 镜像以 `pip install ./sidecar` 安装，此处是唯一钉版源）。
- 独立调试单个任务：`python -m agent_sidecar --job-file <任务 JSON>`（任务帧可附带
  `chapterContent` 字段供回调工具使用；调试模式不支持章节搜索）。
- `python -m agent_sidecar` 必须在 `sidecar/` 目录下运行（模块解析依赖 cwd），
  与后端 `AGENT_SIDECAR_COMMAND` 默认行为一致。

## 测试口径（pytest 90 例）

| 分块 | 数量 | 来源 |
| --- | --- | --- |
| 领域规则（`aiBookGeneration` 可测部分） | 12 | 从旧前端 vitest 用例 **1:1 移植**（prompt 组装、工具调用跑通、归一化去重、worldview 防复述、增量合并、summary 封顶、地图再生判定、降级结构） |
| `aiBookNormalize` 原语 | 8 | vitest 1:1 移植 |
| `aiBookChapterFilter` | 3 + 2 | vitest 1:1 移植 + 窗口化（非零起始数组）新用例 |
| `httpError` 摘要 | 3 | vitest 1:1 移植（另加脱敏用例） |
| 传输层重写 | ~10 | 旧版 fetch 三路径（直连/代理/server）已废除，重写为 model_client 的 URL/鉴权/重试/错误摘要用例 |
| 新编排能力 | 其余 | 轮次预算与强制提交、get_chapter 分窗、search 钳制、merges 归并、输入预算、redraw_map —— 计划新增，无旧版对应 |

## 与旧 TS 实现的一致性验证（历史记录）

移植期间建立过一条差分防线：`tests/differential/cases.json` 里的纯函数用例
（normalize / summary / chapter_filter / json_extract / httpError / map_prompt /
coerce_model_update 共 64 例）分别由 TS 实现与 Python 实现计算并逐例比对，
M1 阶段 **65/65 通过**（含 1 例覆盖数校验）。该防线是一次性设施：M2 推翻式切换
删除了旧 TS 实现（`aiBookGeneration.ts` 等），差分脚本与 actuals 随之移除；
此后两侧口径的一致性由上述 1:1 移植用例与代码评审保障——
**改动 normalize/词表时必须两侧同步**（前端 `aiBookNormalize.ts` 是展示层仍在用的副本）。

## 纪律

- stdout 只写 NDJSON 帧（写后 flush）；日志一律走 stderr（Rust 侧持续排空进 tracing）。
- stdin EOF 时进程必须自行退出（宿主崩溃不留孤儿）。
- 错误消息出 sidecar 前一律过 `errors.sanitize_error_message`（key 脱敏）。
