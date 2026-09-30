"""协议帧与工具回调配对。"""

import io
import json

import pytest

from agent_sidecar import protocol
from agent_sidecar.protocol import (
    HostGoneError,
    ProtocolError,
    StdioChannel,
    hello,
    result_error,
    result_ok,
)


def test_hello_frame_shape():
    frame = hello()
    assert frame["type"] == "hello"
    assert frame["protocol"] == 1
    assert "sidecar" in frame


def test_result_frames():
    ok = result_ok("job-1", {"memory": {}, "shouldRegenerateMap": False})
    assert ok["type"] == "result" and ok["ok"] is True and ok["jobId"] == "job-1"
    err = result_error("job-2", " boom ")
    assert err["type"] == "result" and err["ok"] is False and err["error"] == " boom "


def test_send_writes_single_line_compact_json_without_ascii_escapes():
    out = io.StringIO()
    channel = StdioChannel(stdin=io.StringIO(""), stdout=out)
    channel.send({"type": "status", "phase": "text", "message": "更新 北境 的 AI 资料..."})
    lines = out.getvalue().splitlines()
    assert len(lines) == 1
    assert "更新 北境 的 AI 资料..." in lines[0]
    assert "\\u" not in lines[0]
    assert json.loads(lines[0])["phase"] == "text"


def test_read_frame_returns_none_on_eof():
    channel = StdioChannel(stdin=io.StringIO(""), stdout=io.StringIO())
    assert channel.read_frame() is None


def test_read_frame_skips_garbage_and_blank_lines():
    stream = io.StringIO('not json\n\n{"type":"run","jobId":"j"}\n')
    channel = StdioChannel(stdin=stream, stdout=io.StringIO())
    assert channel.read_frame() == {"type": "run", "jobId": "j"}


def _patch_uuid(monkeypatch) -> None:
    monkeypatch.setattr(protocol.uuid, "uuid4", lambda: type("U", (), {"hex": "abc123"})())


def test_call_host_tool_pairs_request_and_response(monkeypatch):
    _patch_uuid(monkeypatch)
    out = io.StringIO()
    responses = iter(['{"type":"tool_result","id":"call-abc123","content":{"ok":true,"content":"正文"}}\n'])

    class ScriptedStdin:
        def readline(self):
            return next(responses, "")

    channel = StdioChannel(stdin=ScriptedStdin(), stdout=out)
    result = channel.call_host_tool("get_completed_chapter", {"index": 3})
    assert result == {"ok": True, "content": "正文"}

    sent = json.loads(out.getvalue().splitlines()[-1])
    assert sent == {
        "type": "tool_call",
        "id": "call-abc123",
        "name": "get_completed_chapter",
        "args": {"index": 3},
    }


def test_call_host_tool_rejects_mismatched_id(monkeypatch):
    _patch_uuid(monkeypatch)
    channel = StdioChannel(stdin=io.StringIO('{"type":"tool_result","id":"other","content":{}}\n'), stdout=io.StringIO())
    with pytest.raises(ProtocolError, match="id 不匹配"):
        channel.call_host_tool("search_read_content", {"keyword": "x"})


def test_call_host_tool_rejects_wrong_frame_type(monkeypatch):
    _patch_uuid(monkeypatch)
    channel = StdioChannel(stdin=io.StringIO('{"type":"status","phase":"text"}\n'), stdout=io.StringIO())
    with pytest.raises(ProtocolError, match="tool_result"):
        channel.call_host_tool("search_read_content", {"keyword": "x"})


def test_call_host_tool_raises_when_host_gone(monkeypatch):
    _patch_uuid(monkeypatch)
    channel = StdioChannel(stdin=io.StringIO(""), stdout=io.StringIO())
    with pytest.raises(HostGoneError):
        channel.call_host_tool("search_read_content", {"keyword": "x"})
