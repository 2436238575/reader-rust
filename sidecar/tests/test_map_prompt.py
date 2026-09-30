"""地图提示词：制图约束包装（vitest 用例移植）+ 兜底提示词。"""

from agent_sidecar.map_prompt import build_fallback_map_prompt, build_map_image_prompt, request_map_image

RAW_PROMPT = (
    "绘制一张包含两个独立区域的地图：左侧为现代化的地球大学机房，"
    "右侧为荒凉废土中的404号避难所，两者之间以虚线连接。"
)


def test_wraps_prompts_with_cartographic_constraints():
    wrapped = build_map_image_prompt(RAW_PROMPT)
    assert wrapped != RAW_PROMPT
    assert RAW_PROMPT in wrapped
    assert "俯视地图" in wrapped
    assert "地图符号" in wrapped
    assert "不要生成写实照片" in wrapped
    assert "不要画人物" in wrapped
    assert "机房、避难所等室内或建筑地点只能表现为地图上的标注区域" in wrapped


def test_empty_prompt_gets_fallback_source_line():
    wrapped = build_map_image_prompt("   ")
    assert "根据已读进度中的已知地点绘制小说世界地图。" in wrapped


def test_fallback_map_prompt_uses_locations_then_summary():
    memory = {
        "locations": [
            {"name": "北境", "kind": "区域", "description": "寒冷边境。"},
            {"name": "莫雷蒂公寓", "parentName": "廷根市", "kind": "住宅", "description": "两居室。"},
        ],
        "summary": "主角在廷根市活动。",
    }
    prompt = build_fallback_map_prompt(memory, {"name": "山海旧事"})
    assert "为小说《山海旧事》绘制一张不剧透的世界地图。" in prompt
    assert "廷根市 > 莫雷蒂公寓（住宅）: 两居室。" in prompt
    assert "北境" in prompt


def test_fallback_map_prompt_falls_back_to_summary():
    prompt = build_fallback_map_prompt({"locations": [], "summary": "主角在北境。"}, {"name": "X"})
    assert "主角在北境。" in prompt


def test_fallback_map_prompt_uses_scroll_style_when_nothing_known():
    prompt = build_fallback_map_prompt({"locations": [], "summary": ""}, {"name": "X"})
    assert "保留未知区域，以卷轴地图风格呈现。" in prompt


def test_sensenova_always_gets_watermark_false():
    """sensenova 系列模型始终传 watermark=False；size 里的空格被清理。"""
    import httpx
    from agent_sidecar.model_client import ModelClient
    from agent_sidecar.types import ImageModelEndpoint

    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        import json as _json
        captured["body"] = _json.loads(request.content)
        return httpx.Response(200, json={"data": [{"b64_json": "aGk="}]})

    client = ModelClient(transport=httpx.MockTransport(handler))
    endpoint = ImageModelEndpoint(
        baseUrl="https://token.sensenova.cn",
        apiKey="k",
        model="sensenova-u1.5-lite",
        imageSize="2048 x 2048",
    )
    request_map_image(client, endpoint, "p")
    assert captured["body"]["watermark"] is False
    assert captured["body"]["size"] == "2048x2048"

    # 非 sensenova 不传 watermark
    captured.clear()
    other = ImageModelEndpoint(baseUrl="https://api.openai.com", apiKey="k", model="dall-e-3")
    request_map_image(client, other, "p")
    assert "watermark" not in captured["body"]
