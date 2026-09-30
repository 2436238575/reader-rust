"""共享测试设施：任务构造器、假 stdio 通道、脚本化模型客户端。"""

from __future__ import annotations

import io
import json
from typing import Any

import pytest

from agent_sidecar.protocol import StdioChannel
from agent_sidecar.types import JobRequest


def make_job(**overrides: Any) -> JobRequest:
    job: dict[str, Any] = {
        "jobId": "job-1",
        "kind": "chapter_update",
        "book": {"name": "诡秘之主", "author": "爱潜水的乌贼", "bookUrl": "book-1"},
        "chapter": {"index": 10, "title": "第十一章", "url": "chapter-11"},
        "chapters": [],
        "memory": {
            "bookUrl": "book-1",
            "enabled": True,
            "updatedAt": 0,
            "worldview": [],
            "characters": [],
            "relationships": [],
            "locations": [],
        },
        "model": {
            "text": {
                "baseUrl": "http://localhost:8825",
                "apiKey": "",
                "model": "gpt-4o-mini",
                "useFullUrl": False,
            },
            "image": {
                "baseUrl": "http://localhost:8826",
                "apiKey": "image-key",
                "model": "gpt-image-1",
                "imageSize": "1024x1024",
                "useFullUrl": False,
            },
        },
    }
    for key, value in overrides.items():
        if key in ("memory", "model", "book", "chapter") and isinstance(value, dict):
            job[key] = {**job[key], **value}
        else:
            job[key] = value
    return JobRequest.model_validate(job)


def tool_call_response(*calls: tuple[str, dict]) -> dict:
    return {
        "choices": [
            {
                "message": {
                    "tool_calls": [
                        {
                            "id": f"call-{index}",
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": json.dumps(args, ensure_ascii=False),
                            },
                        }
                        for index, (name, args) in enumerate(calls)
                    ]
                }
            }
        ]
    }


def content_response(payload: dict, trailing: str = "") -> dict:
    return {
        "choices": [
            {"message": {"content": json.dumps(payload, ensure_ascii=False) + trailing}}
        ]
    }


class FakeChannel(StdioChannel):
    """记录 send 帧；工具回调按预置队列应答（跳过 id 配对，配对在 test_protocol 单测）。"""

    def __init__(self, tool_results: list[dict] | None = None) -> None:
        super().__init__(stdin=io.StringIO(""), stdout=io.StringIO())
        self.sent: list[dict] = []
        self.tool_calls: list[tuple[str, dict]] = []
        self._tool_results = list(tool_results or [])

    def send(self, frame: dict) -> None:  # type: ignore[override]
        self.sent.append(frame)
        if frame.get("type") == "tool_call":
            self.tool_calls.append((frame.get("name"), frame.get("args")))

    def read_frame(self) -> dict | None:  # type: ignore[override]
        if self._tool_results:
            return self._tool_results.pop(0)
        return None

    def call_host_tool(self, name: str, args: dict) -> dict:  # type: ignore[override]
        self.tool_calls.append((name, args))
        frame = self._tool_results.pop(0) if self._tool_results else None
        if frame is None:
            from agent_sidecar.protocol import HostGoneError

            raise HostGoneError("调试队列已空")
        content = frame.get("content")
        return content if isinstance(content, dict) else {"ok": False, "error": "空工具结果"}


class ScriptedClient:
    """按脚本返回 chat/images 响应，记录每次请求 payload（深拷贝快照，
    模拟真实 HTTP 客户端在发送时刻的序列化视图——messages 列表会在轮次间继续变化）。"""

    def __init__(self, responses: list[dict]) -> None:
        self.responses = list(responses)
        self.requests: list[dict] = []

    def _record(self, endpoint, payload: dict) -> None:
        self.requests.append(json.loads(json.dumps(payload, ensure_ascii=False)))

    def chat(self, endpoint, payload: dict) -> dict:
        self._record(endpoint, payload)
        return self.responses.pop(0)

    def images(self, endpoint, payload: dict) -> dict:
        self._record(endpoint, payload)
        return self.responses.pop(0)


@pytest.fixture
def base_memory() -> dict:
    return {
        "bookUrl": "book-1",
        "enabled": True,
        "updatedAt": 0,
        "worldview": [],
        "characters": [],
        "relationships": [],
        "locations": [],
    }
