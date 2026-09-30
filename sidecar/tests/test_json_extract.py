"""JSON 提取状态机（aiBookGeneration.ts:1118-1164 行为对齐）。"""

import pytest

from agent_sidecar.json_extract import extract_first_json_object, parse_json_content


def test_strips_leading_json_fence():
    assert extract_first_json_object('```json\n{"a": 1}\n```') == '{"a": 1}'
    assert extract_first_json_object('```\n{"a": 1}') == '{"a": 1}'


def test_accepts_trailing_explanation_text():
    text = '{"memory": {"summary": "主角抵达北境。"}}\n\n说明：已按当前章节更新。'
    assert extract_first_json_object(text) == '{"memory": {"summary": "主角抵达北境。"}}'


def test_braces_inside_strings_do_not_break_matching():
    text = '{"s": "包含 } 与 { 的字符串", "b": 2}'
    assert extract_first_json_object(text) == text


def test_raises_when_no_object_present():
    with pytest.raises(ValueError, match="未包含 JSON 对象"):
        extract_first_json_object("抱歉，我无法输出结构化结果。")


def test_raises_when_object_incomplete():
    with pytest.raises(ValueError, match="JSON 对象不完整"):
        extract_first_json_object('{"memory": {"summary": "未闭合')


def test_parse_json_content_reports_invalid_json():
    with pytest.raises(ValueError, match="不是有效 JSON"):
        parse_json_content('{"memory": invalid}')
    parsed = parse_json_content('{"memory": {"summary": "ok"}}')
    assert parsed["memory"]["summary"] == "ok"


def test_parse_json_content_rejects_non_object():
    # 提取器只认花括号对象，数组输入在提取阶段即报错
    with pytest.raises(ValueError, match="未包含 JSON 对象"):
        parse_json_content("[1, 2, 3]")
