"""从模型混合输出中提取第一个完整 JSON 对象。

移植自 aiBookGeneration.ts:1118-1164。只做两件事：剥掉开头的 ```json 围栏、
括号配对截取——刻意不做「补引号」之类的 JSON 修复，坏输出宁可报错。
"""

from __future__ import annotations

import json
import re

_FENCE_RE = re.compile(r"^```(?:json)?\s*", re.IGNORECASE)


def extract_first_json_object(content: str) -> str:
    text = _FENCE_RE.sub("", content, count=1).strip()
    start = text.find("{")
    if start < 0:
        raise ValueError("AI 资料生成结果未包含 JSON 对象")

    depth = 0
    in_string = False
    escaped = False
    for index in range(start, len(text)):
        char = text[index]
        if in_string:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            continue

        if char == '"':
            in_string = True
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return text[start : index + 1]

    raise ValueError("AI 资料生成结果 JSON 对象不完整")


def parse_json_content(content: str) -> dict:
    trimmed = content.strip()
    json_text = extract_first_json_object(trimmed)
    try:
        parsed = json.loads(json_text)
    except json.JSONDecodeError as error:
        raise ValueError(f"AI 资料生成结果不是有效 JSON：{error}") from error
    if not isinstance(parsed, dict):
        raise ValueError("AI 资料生成结果必须是 JSON 对象")
    return parsed
