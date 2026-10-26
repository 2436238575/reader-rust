"""工具执行分发：宿主断连（HostGoneError）必须穿透，不能被吞成工具错误回喂模型。"""

import json

import pytest

from agent_sidecar.protocol import HostGoneError
from agent_sidecar.tools import ToolContext, execute_tool_call
from agent_sidecar.types import BookRef, ChapterRef
from conftest import FakeChannel


def _tool_call_frame(name: str, arguments: dict) -> dict:
    return {
        "id": "call-1",
        "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments)},
    }


def _ctx_with_dead_host() -> ToolContext:
    # FakeChannel 的工具结果队列为空时 call_host_tool 抛 HostGoneError，
    # 等价于宿主在回调期间关闭了管道
    return ToolContext(
        book=BookRef(name="书", author="作者", bookUrl="book-1"),
        chapter=ChapterRef(index=0, title="第一章", url="chapter-1"),
        chapters=[],
        memory={"processedChapterIndex": 3},
        channel=FakeChannel(),
    )


def test_get_chapter_reraises_host_gone():
    with pytest.raises(HostGoneError):
        execute_tool_call(_tool_call_frame("get_completed_chapter", {"index": 0}), _ctx_with_dead_host())


def test_search_content_reraises_host_gone():
    with pytest.raises(HostGoneError):
        execute_tool_call(
            _tool_call_frame("search_read_content", {"keyword": "暗号"}), _ctx_with_dead_host()
        )


def test_ordinary_callback_errors_are_still_returned_as_tool_errors():
    # 对照组：普通回调失败仍是工具错误（回喂模型），不误伤既有语义
    class BrokenChannel(FakeChannel):
        def call_host_tool(self, name, args, job_id=None):
            raise RuntimeError("连接重置")

    ctx = ToolContext(
        book=BookRef(name="书", author="作者", bookUrl="book-1"),
        chapter=ChapterRef(index=0, title="第一章", url="chapter-1"),
        chapters=[],
        memory={"processedChapterIndex": 3},
        channel=BrokenChannel(),
    )
    outcome = execute_tool_call(_tool_call_frame("get_completed_chapter", {"index": 0}), ctx)
    assert outcome.content["ok"] is False
    assert "章节内容获取失败" in outcome.content["error"]
