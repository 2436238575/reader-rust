"""名称/重要度归一化原语。

移植自 frontend/src/utils/aiBookNormalize.ts，词表与判定口径逐字保留——
它们是展示层（前端同名模块）与生成管线（本包 merge.py）共用的语义约定，
"顺手改进"任何一条都会造成前后端口径漂移。刻意不做 NFKC / 全角折叠。
"""

from __future__ import annotations

import re

_MIDDOT_RE = re.compile("[·•・]")
_WHITESPACE_RE = re.compile(r"\s+")

LOW_IMPORTANCE_TERMS = [
    "low",
    "低",
    "低重要性",
    "不重要",
    "路人",
    "背景",
    "minor",
    "background",
    "oneoff",
    "一次性",
]


def normalize_key(value: str | None) -> str:
    """归一化键：忽略大小写/空白/中点变体，用于名称去重与相等判断。"""
    if not value:
        return ""
    return _WHITESPACE_RE.sub("", _MIDDOT_RE.sub(".", value.strip().lower()))


def is_low_importance(value: str | None) -> bool:
    key = normalize_key(value)
    if not key:
        return False
    return any(term in key for term in LOW_IMPORTANCE_TERMS)


def importance_rank(value: str | None) -> int:
    """0=未标注/未知 1=低 2=中 3=高。注意是子串包含，先判高再判中。"""
    key = normalize_key(value)
    if "high" in key or "高" in key:
        return 3
    if "medium" in key or "中" in key:
        return 2
    if is_low_importance(value):
        return 1
    return 0


def richer_string(current: str | None, next_value: str | None) -> str:
    """取内容更充实的那个（长者为优），等长保留 current。"""
    if not current:
        return next_value or ""
    if not next_value:
        return current
    return next_value if len(next_value) > len(current) else current


def prefer_importance(current: str | None, next_value: str | None) -> str | None:
    if importance_rank(next_value) > importance_rank(current):
        return next_value
    return current or next_value


def unique_strings(values: list[str] | None) -> list[str]:
    """按归一化键去重，空 key 跳过，保留首次出现的原文。"""
    seen: set[str] = set()
    result: list[str] = []
    for value in values or []:
        key = normalize_key(value)
        if not key or key in seen:
            continue
        seen.add(key)
        result.append(value)
    return result


def relationship_key(source: str, target: str, relation: str) -> str:
    """关系去重键：无向点对 + 关系词。"""
    pair = sorted([normalize_key(source), normalize_key(target)])
    return f"{pair[0]}::{pair[1]}::{normalize_key(relation)}"


WEAK_RELATIONS = ["认识", "见过", "路过", "同村", "同校", "位于", "相关"]


def is_low_value_relationship(relation: str, detail: str, importance: str | None) -> bool:
    """「认识/见过」这类弱关系且描述过短（<18 字）时视为低价值噪音边。"""
    key = normalize_key(importance)
    if "high" in key or "medium" in key or "高" in key or "中" in key:
        return False
    if normalize_key(relation) not in WEAK_RELATIONS:
        return False
    return len(normalize_key(detail)) < 18
