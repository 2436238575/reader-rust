"""地图提示词：制图约束包装（vitest 用例移植）+ 兜底提示词。"""

from agent_sidecar.map_prompt import build_fallback_map_prompt, build_map_image_prompt

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
