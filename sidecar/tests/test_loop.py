"""agent 循环：golden 流程（脚本化模型响应）+ 新能力（跳过/强制提交/搜索钳制/merges/预算）。"""

import json

import pytest

from agent_sidecar import loop as loop_module
from agent_sidecar.loop import run_job
from agent_sidecar.prompts import FORCED_COMMIT_MESSAGE

from conftest import FakeChannel, ScriptedClient, content_response, make_job, tool_call_response

CHAPTER_CONTENT = "刘隆指出李皓已经接触超凡领域。"


def _chapter_tool_result(content: str = CHAPTER_CONTENT) -> dict:
    return {
        "type": "tool_result",
        "id": "ignored",
        "content": {"ok": True, "content": content, "totalLength": len(content)},
    }


def test_runs_memory_updates_through_tool_calling(base_memory):
    save_patch = {
        "memory": {
            "summary": "主角确认超凡领域存在。",
            "worldview": [
                {
                    "category": "基础规则",
                    "title": "超凡领域",
                    "content": "存在以特殊能力影响现实的超凡领域，细节仍未公开。",
                    "confidence": "已知",
                    "importance": "high",
                }
            ],
            "characters": [],
            "relationships": [],
            "locations": [],
        },
        "shouldRegenerateMap": False,
    }
    client = ScriptedClient(
        [
            tool_call_response(
                ("get_current_memory", {}),
                ("get_completed_chapter", {}),
            ),
            tool_call_response(("save_memory_patch", save_patch)),
        ]
    )
    channel = FakeChannel(tool_results=[_chapter_tool_result()])
    job = make_job(memory={**base_memory, "summary": "旧资料摘要"})

    result = run_job(job, channel, client)

    assert len(client.requests) == 2
    first_body = json.dumps(client.requests[0], ensure_ascii=False)
    assert [t["function"]["name"] for t in client.requests[0]["tools"]] == [
        "get_current_memory",
        "get_completed_chapter",
        "search_read_content",
        "save_memory_patch",
    ]
    # 防剧透：首请求不含记忆与章节正文，第二轮（工具结果进上下文后）才可见
    assert "旧资料摘要" not in first_body
    assert CHAPTER_CONTENT not in first_body
    second_body = json.dumps(client.requests[1], ensure_ascii=False)
    assert "旧资料摘要" in second_body
    assert CHAPTER_CONTENT in second_body

    assert result["memory"]["summary"] == "主角确认超凡领域存在。"
    assert result["memory"]["processedChapterIndex"] == 10
    assert result["memory"]["worldview"][0]["category"] == "基础规则"
    assert result["memory"]["worldview"][0]["title"] == "超凡领域"
    assert result["shouldRegenerateMap"] is False

    # 事件：status + round ×2
    assert channel.sent[0]["type"] == "status" and channel.sent[0]["phase"] == "text"
    rounds = [f for f in channel.sent if f["type"] == "round"]
    assert [r["step"] for r in rounds] == [1, 2]
    assert rounds[0]["toolCalls"] == ["get_current_memory", "get_completed_chapter"]


def test_accepts_model_json_content_with_trailing_explanation(base_memory):
    client = ScriptedClient(
        [
            content_response(
                {
                    "memory": {"summary": "主角抵达北境。"},
                    "shouldRegenerateMap": False,
                },
                trailing="\n\n说明：已按当前章节更新。",
            )
        ]
    )
    job = make_job(
        chapter={"index": 7, "title": "第八章", "url": "chapter-8"},
        memory=base_memory,
    )
    result = run_job(job, FakeChannel(), client)
    assert result["memory"]["summary"] == "主角抵达北境。"
    assert result["memory"]["processedChapterIndex"] == 7


def test_skips_non_story_chapter_without_any_model_call(base_memory):
    client = ScriptedClient([])
    job = make_job(
        chapter={"index": 22, "title": "番外：普通人的日常", "url": "c"},
        chapters=[{"index": 22, "title": "番外：普通人的日常"}],
        memory=base_memory,
    )
    result = run_job(job, FakeChannel(), client)
    assert result == {"skipped": True}
    assert client.requests == []


def test_skip_check_runs_before_model_config_check(base_memory):
    """跳过判定先于模型就绪检查——跳过章零流量。"""
    client = ScriptedClient([])
    job = make_job(
        chapter={"index": 22, "title": "上架感言", "url": "c"},
        chapters=[{"index": 22, "title": "上架感言"}],
        memory=base_memory,
        model={"text": {"baseUrl": "", "model": ""}},
    )
    assert run_job(job, FakeChannel(), client) == {"skipped": True}


def test_unready_text_model_raises_model_error(base_memory):
    client = ScriptedClient([])
    job = make_job(memory=base_memory, model={"text": {"baseUrl": "", "model": ""}})
    with pytest.raises(Exception, match="文本模型未配置"):
        run_job(job, FakeChannel(), client)


def test_forced_commit_message_injected_on_final_round(base_memory, monkeypatch):
    monkeypatch.setattr(loop_module, "MAX_ROUNDS", 2)
    save_patch = {"memory": {"summary": "兜底摘要。"}, "shouldRegenerateMap": False}
    client = ScriptedClient(
        [
            tool_call_response(("get_current_memory", {})),
            tool_call_response(("save_memory_patch", save_patch)),
        ]
    )
    channel = FakeChannel(tool_results=[_chapter_tool_result()])
    result = run_job(make_job(memory=base_memory), channel, client)
    assert result["memory"]["summary"] == "兜底摘要。"
    assert FORCED_COMMIT_MESSAGE[:12] in json.dumps(client.requests[-1], ensure_ascii=False)


def test_input_budget_exhaustion_triggers_forced_commit(base_memory, monkeypatch):
    monkeypatch.setattr(loop_module, "MAX_INPUT_CHARS", 10)
    save_patch = {"memory": {"summary": "预算触顶摘要。"}, "shouldRegenerateMap": False}
    client = ScriptedClient(
        [
            tool_call_response(("get_current_memory", {})),
            tool_call_response(("save_memory_patch", save_patch)),
        ]
    )
    channel = FakeChannel(tool_results=[_chapter_tool_result()])
    result = run_job(make_job(memory=base_memory), channel, client)
    assert result["memory"]["summary"] == "预算触顶摘要。"
    assert FORCED_COMMIT_MESSAGE[:12] in json.dumps(client.requests[-1], ensure_ascii=False)


def test_round_limit_without_final_patch_raises(base_memory, monkeypatch):
    monkeypatch.setattr(loop_module, "MAX_ROUNDS", 2)
    client = ScriptedClient(
        [
            tool_call_response(("get_current_memory", {})),
            tool_call_response(("get_current_memory", {})),
        ]
    )
    channel = FakeChannel(tool_results=[_chapter_tool_result(), _chapter_tool_result()])
    with pytest.raises(Exception, match="轮次限制"):
        run_job(make_job(memory=base_memory), channel, client)


def test_search_callback_is_clamped_to_processed_progress(base_memory):
    memory = {**base_memory, "processedChapterIndex": 10}
    client = ScriptedClient(
        [
            tool_call_response(("search_read_content", {"keyword": "林青云", "toIndex": 99})),
            content_response({"memory": {"summary": "查证完成。"}, "shouldRegenerateMap": False}),
        ]
    )
    channel = FakeChannel(
        tool_results=[
            {
                "type": "tool_result",
                "id": "x",
                "content": {
                    "ok": True,
                    "matches": [{"chapterIndex": 3, "chapterTitle": "第四章", "snippet": "林青云出场"}],
                },
            }
        ]
    )
    result = run_job(make_job(memory=memory), channel, client)
    name, args = channel.tool_calls[0]
    assert name == "search_read_content"
    assert args["toIndex"] == 10  # 模型要求 99，被钳到已处理进度
    assert args["fromIndex"] == 0
    assert result["memory"]["summary"] == "查证完成。"


def test_merges_protocol_flows_through_save_patch(base_memory):
    save_patch = {
        "memory": {
            "summary": "林师兄与林青云实为一人。",
            "characters": [
                {"name": "林青云", "status": "宗门弟子", "importance": "high"},
                {"name": "林师兄", "status": "在藏书阁当值", "importance": "high"},
            ],
            "relationships": [
                {"source": "林师兄", "target": "长老", "relation": "师徒", "description": "亲传弟子。", "importance": "high"}
            ],
        },
        "shouldRegenerateMap": False,
        "merges": [{"canonical": "林青云", "absorbs": ["林师兄"]}],
    }
    client = ScriptedClient([tool_call_response(("save_memory_patch", save_patch))])
    job = make_job(
        memory={
            **base_memory,
            "processedChapterIndex": 7,
            "relationships": [],
        }
    )
    result = run_job(job, FakeChannel(), client)
    assert [item["name"] for item in result["memory"]["characters"]] == ["林青云"]
    assert "林师兄" in result["memory"]["characters"][0]["aliases"]
    assert result["memory"]["relationships"][0]["source"] == "林青云"


def test_get_chapter_window_reports_truncation(base_memory):
    content = "正文字符" * 10000  # 50000 字符
    client = ScriptedClient(
        [
            tool_call_response(("get_completed_chapter", {})),
            tool_call_response(
                ("save_memory_patch", {"memory": {"summary": "读完了。"}, "shouldRegenerateMap": False})
            ),
        ]
    )
    channel = FakeChannel(
        tool_results=[
            _chapter_tool_result(content),
        ]
    )
    run_job(make_job(memory=base_memory), channel, client)
    # 两次请求之间的工具结果内容都是完整正文，分窗发生在 sidecar 侧：
    # 模型收到的第一份工具结果应被截到默认窗口并在第二轮可续读
    first_tool_payload = json.loads(
        [m for m in client.requests[1]["messages"] if m.get("role") == "tool"][0]["content"]
    )
    assert first_tool_payload["truncated"] is True
    assert first_tool_payload["totalLength"] == 40000
    assert len(first_tool_payload["chapter"]["content"]) == 24000


def test_redraw_map_uses_existing_prompt_then_image_model(base_memory):
    memory = {
        **base_memory,
        "processedChapterIndex": 9,
        "map": {"prompt": "绘制旧村地图。", "updatedAt": 100},
    }
    client = ScriptedClient([{"data": [{"b64_json": "aGk="}]}])
    job = make_job(kind="redraw_map", chapter=None, memory=memory)
    result = run_job(job, FakeChannel(), client)
    assert result["mapPrompt"] == "绘制旧村地图。"
    assert result["map"] == {"b64Json": "aGk=", "imageUrl": None}
    assert result["sourceChapterIndex"] == 9
    body = json.dumps(client.requests[0], ensure_ascii=False)
    assert "俯视地图" in body
    assert client.requests[0]["model"] == "gpt-image-1"


def test_redraw_map_builds_fallback_prompt_from_locations(base_memory):
    memory = {
        **base_memory,
        "processedChapterIndex": 9,
        "locations": [{"name": "北境", "kind": "区域", "description": "寒冷边境。"}],
    }
    client = ScriptedClient([{"data": [{"url": "https://cdn.example.test/map.png"}]}])
    result = run_job(make_job(kind="redraw_map", chapter=None, memory=memory), FakeChannel(), client)
    assert "为小说《诡秘之主》绘制一张不剧透的世界地图。" in result["mapPrompt"]
    assert "北境" in result["mapPrompt"]
    assert result["map"]["imageUrl"] == "https://cdn.example.test/map.png"


def test_redraw_map_without_image_model_raises(base_memory):
    client = ScriptedClient([])
    job = make_job(
        kind="redraw_map",
        chapter=None,
        memory=base_memory,
        model={"image": {"baseUrl": "", "model": ""}},
    )
    with pytest.raises(Exception, match="图片模型未配置"):
        run_job(job, FakeChannel(), client)


def test_save_patch_without_memory_key_is_rejected(base_memory):
    """旧别名（patch/memoryPatch）已废除：模型用错键名时显式报错，禁止整包落库。"""
    from agent_sidecar.tools import ToolContext, execute_tool_call

    ctx = ToolContext(
        book=make_job().book,
        chapter=make_job().chapter,
        chapters=[],
        memory=base_memory,
        channel=None,
    )
    tool_call = {
        "id": "call-1",
        "function": {
            "name": "save_memory_patch",
            "arguments": json.dumps(
                {"patch": {"summary": "整包污染"}}, ensure_ascii=False
            ),
        },
    }
    outcome = execute_tool_call(tool_call, ctx)
    assert outcome.final is False
    assert outcome.content["ok"] is False
    assert "memory 键" in outcome.content["error"]
