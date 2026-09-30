"""stdio NDJSON 通道与帧构造。

纪律：
- stdout 只写行式 JSON（ensure_ascii=False、紧凑分隔符），写后必须 flush；
- 日志一律走 stderr（__main__ 负责 logging 配置）；
- read_frame 在 EOF 时返回 None——sidecar 依赖它感知宿主退出并自行结束，
  这是「Rust 崩溃不留孤儿进程」约定的一半（另一半是 Rust 侧 kill + watchdog）；
- 工具回调是单飞配对：发出 tool_call 后阻塞等同 id 的 tool_result。
"""

from __future__ import annotations

import json
import logging
import sys
import uuid
from typing import Any, TextIO

from . import PROTOCOL_VERSION, SIDECAR_VERSION
from .types import JobRequest

log = logging.getLogger(__name__)


class ProtocolError(Exception):
    pass


class HostGoneError(Exception):
    pass


def _dump(frame: dict[str, Any]) -> str:
    return json.dumps(frame, ensure_ascii=False, separators=(",", ":"))


class StdioChannel:
    def __init__(self, stdin: TextIO | None = None, stdout: TextIO | None = None) -> None:
        self._stdin = stdin if stdin is not None else sys.stdin
        self._stdout = stdout if stdout is not None else sys.stdout

    # -- 输出 ---------------------------------------------------------------

    def send(self, frame: dict[str, Any]) -> None:
        self._stdout.write(_dump(frame) + "\n")
        self._stdout.flush()

    # -- 输入 ---------------------------------------------------------------

    def read_frame(self) -> dict[str, Any] | None:
        """读下一帧。EOF 返回 None；空行与不可解析行跳过（不杀进程）。"""
        while True:
            line = self._stdin.readline()
            if line == "":
                return None
            line = line.strip()
            if not line:
                continue
            try:
                frame = json.loads(line)
            except json.JSONDecodeError:
                log.warning("忽略无法解析的输入行（长度=%d）", len(line))
                continue
            if not isinstance(frame, dict):
                log.warning("忽略非对象输入帧")
                continue
            return frame

    # -- 工具回调 ------------------------------------------------------------

    def call_host_tool(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        call_id = f"call-{uuid.uuid4().hex[:12]}"
        self.send({"type": "tool_call", "id": call_id, "name": name, "args": args})
        frame = self.read_frame()
        if frame is None:
            raise HostGoneError("宿主在工具回调期间关闭了输入")
        if frame.get("type") != "tool_result":
            raise ProtocolError(f"工具回调收到非 tool_result 帧：{frame.get('type')}")
        if frame.get("id") != call_id:
            raise ProtocolError("工具回调响应 id 不匹配")
        content = frame.get("content")
        return content if isinstance(content, dict) else {}


# -- 帧构造 ------------------------------------------------------------------

def hello() -> dict[str, Any]:
    return {"type": "hello", "protocol": PROTOCOL_VERSION, "sidecar": SIDECAR_VERSION}


def status(phase: str, message: str = "") -> dict[str, Any]:
    return {"type": "status", "phase": phase, "message": message}


def round_event(step: int, tool_names: list[str]) -> dict[str, Any]:
    return {"type": "round", "step": step, "toolCalls": tool_names}


def result_ok(job_id: str, payload: dict[str, Any]) -> dict[str, Any]:
    return {"type": "result", "jobId": job_id, "ok": True, **payload}


def result_error(job_id: str, message: str) -> dict[str, Any]:
    return {"type": "result", "jobId": job_id, "ok": False, "error": message}


def parse_job(frame: dict[str, Any]) -> JobRequest:
    """从 run 帧解析任务载荷；type 不符或载荷缺失即协议错误。"""
    if frame.get("type") != "run":
        raise ProtocolError(f"期望 run 帧，收到：{frame.get('type')}")
    return JobRequest.model_validate(frame.get("job") or {})
