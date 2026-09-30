"""Reader-Rust AI 资料编排 sidecar。

Rust 后端按任务 spawn 本进程，通过 stdin/stdout 的行式 JSON（NDJSON）通信：
任务帧从 stdin 读入，进度事件与结果帧写 stdout，日志一律走 stderr。
"""

PROTOCOL_VERSION = 1
SIDECAR_VERSION = "0.1.0"
