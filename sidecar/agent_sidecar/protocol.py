"""stdio NDJSON 通道与帧构造。

纪律：
- stdout 只写行式 JSON（ensure_ascii=False、紧凑分隔符），写后必须 flush；
- 日志一律走 stderr（__main__ 负责 logging 配置）；
- read_frame 在 EOF 时返回 None——sidecar 依赖它感知宿主退出并自行结束，
  这是「Rust 崩溃不留孤儿进程」约定的一半（另一半是 Rust 侧 kill + watchdog）；
- 输入逐块读取且单行有长度上限（MAX_LINE_BYTES）：对端失控时不能无界把
  整行读进内存（长篇书的 memory 行可达几十 MB）；
- 工具回调是单飞配对：发出 tool_call 后阻塞等同 id 的 tool_result；
- 协议 v2 起 tool_call/tool_result 帧带 jobId：宿主据此校验工具调用归属
  当前任务（jobId 不匹配的调用收到带错误内容的 tool_result，任务不中断）。
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

# 单行输入上限：与 Rust 侧 stdout 泵的上限一致（64MB），超过即协议错误
MAX_LINE_BYTES = 64 * 1024 * 1024
# 逐块读取的块大小（与行上限无关，只影响读入粒度）
_CHUNK_SIZE = 64 * 1024


class ProtocolError(Exception):
    pass


class HostGoneError(Exception):
    pass


def _dump(frame: dict[str, Any]) -> str:
    return json.dumps(frame, ensure_ascii=False, separators=(",", ":"))


def _decode(line: bytes) -> str:
    """完整行按 UTF-8 解码（坏字节替换，交给 JSON 解析跳过）。

    只对按 b"\\n" 切出的完整行调用：\\n 不可能是 UTF-8 多字节序列的
    延续字节，完整行里不会残留被切断的半个字符。
    """
    return line.decode("utf-8", errors="replace")


class StdioChannel:
    def __init__(self, stdin: Any | None = None, stdout: TextIO | None = None) -> None:
        # 默认走二进制 stdin：宿主写的是 UTF-8 字节，显式解码避免 Windows 上
        # Python 对管道按 ANSI 代码页解码把中文帧读坏
        self._stdin = stdin if stdin is not None else sys.stdin.buffer
        self._stdout = stdout if stdout is not None else sys.stdout
        # 未读完的残余字节：按 b"\n" 切出完整行后才一次性 UTF-8 解码，
        # 避免逐块 decode 把跨块的多字节字符（如中文）切成 U+FFFD 静默污染数据
        self._pending: bytes = b""

    # -- 输出 ---------------------------------------------------------------

    def send(self, frame: dict[str, Any]) -> None:
        self._stdout.write(_dump(frame) + "\n")
        self._stdout.flush()

    # -- 输入 ---------------------------------------------------------------

    def _readline_capped(self) -> str | None:
        """逐块读一行（不含换行）。EOF 且无残留返回 None。

        单行超过 MAX_LINE_BYTES 抛 ProtocolError——逐块累积并在超限时立即
        抛错，不会先把对端吐出的整行无界读进内存。
        """
        pending = self._pending
        while True:
            newline = pending.find(b"\n")
            if newline >= 0:
                if newline > MAX_LINE_BYTES:
                    raise ProtocolError(
                        f"输入单行超过 {MAX_LINE_BYTES // (1024 * 1024)}MB 上限，判定对端失控"
                    )
                self._pending = pending[newline + 1 :]
                return _decode(pending[:newline])
            if len(pending) > MAX_LINE_BYTES:
                raise ProtocolError(
                    f"输入单行超过 {MAX_LINE_BYTES // (1024 * 1024)}MB 上限，判定对端失控"
                )
            chunk = self._stdin.read(_CHUNK_SIZE)
            if not chunk:
                self._pending = b""
                return _decode(pending) if pending else None
            # 文本流（测试用 StringIO）统一编码成字节，与二进制路径共用同一缓冲
            pending += chunk if isinstance(chunk, bytes) else chunk.encode("utf-8")
            self._pending = pending

    def read_frame(self) -> dict[str, Any] | None:
        """读下一帧。EOF 返回 None；空行与不可解析行跳过（不杀进程）。"""
        while True:
            line = self._readline_capped()
            if line is None:
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

    def call_host_tool(
        self, name: str, args: dict[str, Any], job_id: str | None = None
    ) -> dict[str, Any]:
        call_id = f"call-{uuid.uuid4().hex[:12]}"
        frame: dict[str, Any] = {"type": "tool_call", "id": call_id, "name": name, "args": args}
        # 协议 v2：归属校验依赖 jobId；调试模式（无 run 帧上下文）省略
        if job_id:
            frame["jobId"] = job_id
        self.send(frame)
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
