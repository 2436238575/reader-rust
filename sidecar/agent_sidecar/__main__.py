"""sidecar 入口。

默认 stdio 模式（Rust spawn）：stdout 只写 NDJSON 帧，日志全走 stderr；
stdin EOF 即退出（宿主崩溃不留孤儿进程）。

--job-file 调试模式：读取单条任务 JSON（可额外带 chapterContent 字段供
回调工具使用，无宿主时不支持章节搜索），执行后退出——用于脱离 Rust
独立验证任务帧与录制响应。
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys

from .loop import run_job
from .model_client import ModelClient, ModelError
from .protocol import StdioChannel, hello, parse_job, result_error, result_ok
from .errors import sanitize_error_message, scrub_credentials
from .types import JobRequest

log = logging.getLogger("agent_sidecar")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="agent_sidecar")
    parser.add_argument("--job-file", help="调试模式：执行单个任务 JSON 后退出")
    args = parser.parse_args(argv)

    logging.basicConfig(
        stream=sys.stderr,
        level=os.environ.get("AGENT_SIDECAR_LOG_LEVEL", "INFO").upper(),
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
    )

    if args.job_file:
        return _run_job_file(args.job_file) or 0

    channel = StdioChannel()
    channel.send(hello())
    while True:
        frame = channel.read_frame()
        if frame is None:
            log.info("stdin EOF，sidecar 退出")
            return 0
        if frame.get("type") != "run":
            log.warning("忽略未知帧类型：%s", frame.get("type"))
            continue
        _handle_run(frame, channel)


def _handle_run(frame: dict, channel: StdioChannel) -> None:
    try:
        job = parse_job(frame)
    except Exception as error:  # noqa: BLE001 — 坏帧也要以 result 帧回应，避免宿主干等
        log.warning("任务帧无效：%s", error)
        # ValidationError 的输入 repr 理论上可能带上 apiKey，出口前脱敏
        channel.send(
            result_error(str(frame.get("jobId") or ""), sanitize_error_message(f"任务帧无效：{error}"))
        )
        return

    client = ModelClient(secrets=[job.model.text.apiKey, job.model.image.apiKey])
    try:
        payload = run_job(job, channel, client)
        channel.send(result_ok(job.jobId, payload))
    except ModelError as error:
        log.warning("任务 %s 失败：%s", job.jobId, error)
        channel.send(result_error(job.jobId, str(error)))
    except Exception as error:  # noqa: BLE001 — 顶层兜底
        log.exception("任务 %s 执行异常", job.jobId)
        channel.send(result_error(job.jobId, scrub_credentials(str(error))))
    finally:
        client.close()


def _run_job_file(path: str) -> int:
    with open(path, "r", encoding="utf-8") as handle:
        frame = json.load(handle)
    if isinstance(frame.get("job"), dict) is False:
        frame = {"type": "run", "job": frame}
    channel = StdioChannel()
    channel.send(hello())
    _handle_run(frame, channel)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
