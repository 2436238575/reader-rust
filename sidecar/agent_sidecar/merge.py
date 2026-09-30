"""模型 patch 与既有资料的合并管线。

移植自 aiBookGeneration.ts:656-1012（注意：四组 normalize*/merge* 的源头
在这个文件，而非 aiBookNormalize.ts——后者只有无方向性原语）。核心语义：

- 合并方向是「生成期 next 优先」（展示期 mergeDisplay* 方向相反，留在前端）；
- 四个实体数组每次都是「旧数组 + 模型增量」拼接后整体重算归一化，
  去重/过滤逻辑全部集中在 normalize* 里；
- enabled 与 map 字段不受模型输出影响；
- merges 是本次新增的别名归并协议：canonical 吸收 absorbs 条目并重定向引用。
"""

from __future__ import annotations

import time
from typing import Any

from .chapter_filter import parse_chinese_number  # noqa: F401  (re-export 便于差分脚本)
from .normalize import (
    is_low_importance,
    is_low_value_relationship,
    normalize_key,
    prefer_importance,
    relationship_key,
    richer_string,
    unique_strings,
)
from .summary import normalize_summary
from .worldview_guard import is_chapter_summary_worldview

UNKNOWN = Any


def is_record(value: object) -> bool:
    return isinstance(value, dict)


def read_string(record: dict, key: str) -> str:
    value = record.get(key)
    return value.strip() if isinstance(value, str) else ""


def read_string_array(record: dict, key: str) -> list[str]:
    value = record.get(key)
    if not isinstance(value, list):
        return []
    return [item.strip() for item in value if isinstance(item, str) and item.strip()]


def read_boolean(record: dict, key: str) -> bool:
    return record.get(key) is True


def prefer_string(current: str | None, next_value: str | None) -> str | None:
    return current or next_value or None


def merge_incremental_items(previous_items: object, next_items: object) -> list:
    previous_array = previous_items if isinstance(previous_items, list) else []
    if isinstance(next_items, list):
        return [*previous_array, *next_items]
    return list(previous_array)


# ---------------------------------------------------------------------------
# 四组 normalize（TS: aiBookGeneration.ts:785-962）
# ---------------------------------------------------------------------------

def normalize_worldview(items: list) -> list[dict]:
    notes: dict[str, dict] = {}
    for item in items:
        if not is_record(item) or is_low_importance(read_string(item, "importance")):
            continue
        title = read_string(item, "title")
        content = read_string(item, "content")
        if not title or not content:
            continue
        category = read_string(item, "category") or "基础设定"
        if is_chapter_summary_worldview(title, content, category):
            continue
        note = {
            "title": title,
            "content": content,
            "category": category,
            "confidence": read_string(item, "confidence") or None,
            "importance": read_string(item, "importance") or None,
        }
        key = f"{normalize_key(category)}::{normalize_key(title)}"
        existing = notes.get(key)
        notes[key] = merge_note(existing, note) if existing else note
    return list(notes.values())


def normalize_characters(items: list) -> list[dict]:
    characters: dict[str, dict] = {}
    for item in items:
        if not is_record(item) or is_low_importance(read_string(item, "importance")):
            continue
        name = read_string(item, "name")
        if not name:
            continue
        character = {
            "name": name,
            "aliases": unique_strings(read_string_array(item, "aliases")),
            "status": read_string(item, "status") or read_string(item, "description") or "状态未知",
            "faction": read_string(item, "faction") or None,
            "location": read_string(item, "location") or None,
            "description": read_string(item, "description") or None,
            "lastSeenChapter": read_string(item, "lastSeenChapter") or None,
            "importance": read_string(item, "importance") or None,
        }
        key = normalize_key(name)
        existing = characters.get(key)
        characters[key] = merge_character(existing, character) if existing else character
    return list(characters.values())


def normalize_relationships(items: list) -> list[dict]:
    relationships: dict[str, dict] = {}
    for item in items:
        if not is_record(item) or is_low_importance(read_string(item, "importance")):
            continue
        source = read_string(item, "source")
        target = read_string(item, "target")
        relation = read_string(item, "relation")
        description = read_string(item, "description")
        status = read_string(item, "status")
        importance = read_string(item, "importance")
        if not source or not target or not relation:
            continue
        if normalize_key(source) == normalize_key(target):
            continue
        if is_low_value_relationship(relation, description or status, importance):
            continue
        relationship = {
            "source": source,
            "target": target,
            "relation": relation,
            "status": status or None,
            "description": description or None,
            "importance": importance or None,
        }
        key = relationship_key(source, target, relation)
        existing = relationships.get(key)
        relationships[key] = merge_relationship(existing, relationship) if existing else relationship
    return list(relationships.values())


def normalize_locations(items: list) -> list[dict]:
    locations: dict[str, dict] = {}
    for item in items:
        if not is_record(item):
            continue
        name = read_string(item, "name")
        if not name:
            continue
        parent_name = read_string(item, "parentName")
        location = {
            "name": name,
            "kind": read_string(item, "kind") or None,
            "parentName": parent_name if parent_name and normalize_key(parent_name) != normalize_key(name) else None,
            "description": read_string(item, "description") or read_string(item, "status") or "",
            "status": read_string(item, "status") or None,
            "relatedCharacters": unique_strings(read_string_array(item, "relatedCharacters")),
            "firstSeenChapter": read_string(item, "firstSeenChapter") or None,
            "importance": read_string(item, "importance") or None,
        }
        key = normalize_key(name)
        existing = locations.get(key)
        locations[key] = merge_location(existing, location) if existing else location
    return list(locations.values())


# ---------------------------------------------------------------------------
# 合并方向：生成期 next 优先（与展示期相反，勿混用）
# ---------------------------------------------------------------------------

def merge_note(current: dict, next_item: dict) -> dict:
    return {
        **current,
        "content": richer_string(current.get("content"), next_item.get("content")),
        "confidence": prefer_string(current.get("confidence"), next_item.get("confidence")),
        "importance": prefer_importance(current.get("importance"), next_item.get("importance")),
    }


def merge_character(current: dict, next_item: dict) -> dict:
    return {
        **current,
        "aliases": unique_strings([*(current.get("aliases") or []), *(next_item.get("aliases") or [])]),
        "status": next_item.get("status") or current.get("status"),
        "faction": next_item.get("faction") or current.get("faction"),
        "location": next_item.get("location") or current.get("location"),
        "description": richer_string(current.get("description"), next_item.get("description")),
        "lastSeenChapter": next_item.get("lastSeenChapter") or current.get("lastSeenChapter"),
        "importance": prefer_importance(current.get("importance"), next_item.get("importance")),
    }


def merge_relationship(current: dict, next_item: dict) -> dict:
    return {
        **current,
        "status": next_item.get("status") or current.get("status"),
        "description": richer_string(current.get("description"), next_item.get("description")),
        "importance": prefer_importance(current.get("importance"), next_item.get("importance")),
    }


def merge_location(current: dict, next_item: dict) -> dict:
    return {
        **current,
        "kind": next_item.get("kind") or current.get("kind"),
        "parentName": next_item.get("parentName") or current.get("parentName"),
        "description": richer_string(current.get("description"), next_item.get("description")),
        "status": next_item.get("status") or current.get("status"),
        "relatedCharacters": unique_strings(
            [*(current.get("relatedCharacters") or []), *(next_item.get("relatedCharacters") or [])]
        ),
        # 注意方向：firstSeenChapter 先到优先，与其他字段相反
        "firstSeenChapter": current.get("firstSeenChapter") or next_item.get("firstSeenChapter"),
        "importance": prefer_importance(current.get("importance"), next_item.get("importance")),
    }


# ---------------------------------------------------------------------------
# 地图再生的准入判定（TS: 752-783）
# ---------------------------------------------------------------------------

def location_signature(locations: list[dict]) -> str:
    return "|".join(
        sorted(
            ":".join(
                [
                    normalize_key(location.get("name")),
                    normalize_key(location.get("parentName")),
                    normalize_key(location.get("kind")),
                    normalize_key(location.get("status")),
                    normalize_key(location.get("description")),
                ]
            )
            for location in locations
        )
    )


def should_accept_map_regeneration(
    requested: bool, map_prompt: str, previous: dict, locations: list[dict]
) -> bool:
    if not requested or not map_prompt:
        return False
    if not previous.get("map"):
        return True
    previous_signature = location_signature(normalize_locations(previous.get("locations") or []))
    return previous_signature != location_signature(locations)


# ---------------------------------------------------------------------------
# merges 协议（新增）：别名归并 + 引用重定向
# ---------------------------------------------------------------------------

def apply_merges(
    characters: list[dict], relationships: list[dict], locations: list[dict], merges: object
) -> tuple[list[dict], list[dict], list[dict]]:
    if not isinstance(merges, list):
        return characters, relationships, locations

    by_key: dict[str, dict] = {}
    order: list[str] = []
    for entry in characters:
        key = normalize_key(read_string(entry, "name"))
        if key and key not in by_key:
            order.append(key)
        by_key.setdefault(key, entry)

    changed = False
    for merge in merges:
        if not is_record(merge):
            continue
        canonical = read_string(merge, "canonical")
        absorbs = read_string_array(merge, "absorbs")
        canon_key = normalize_key(canonical)
        if not canon_key or not absorbs:
            continue
        target = by_key.get(canon_key)
        if target is None:
            target = {"name": canonical, "aliases": [], "status": "状态未知"}
            by_key[canon_key] = target
            order.append(canon_key)
        for absorbed in absorbs:
            abs_key = normalize_key(absorbed)
            if not abs_key or abs_key == canon_key:
                continue
            source = by_key.pop(abs_key, None)
            if source is not None:
                target = merge_character(target, source)
                target["name"] = read_string(target, "name") or canonical
            target["aliases"] = unique_strings(
                [
                    *(target.get("aliases") or []),
                    read_string(source, "name") if source else "",
                    absorbed,
                ]
            )
            # 重定向引用，再走一次关系归一化去重新产生的重复边
            for rel in relationships:
                if normalize_key(read_string(rel, "source")) == abs_key:
                    rel["source"] = read_string(target, "name")
                if normalize_key(read_string(rel, "target")) == abs_key:
                    rel["target"] = read_string(target, "name")
            for loc in locations:
                related = loc.get("relatedCharacters")
                if isinstance(related, list):
                    loc["relatedCharacters"] = unique_strings(
                        [
                            read_string(target, "name") if normalize_key(str(r)) == abs_key else str(r)
                            for r in related
                        ]
                    )
            changed = True
        by_key[canon_key] = target

    if not changed:
        return characters, relationships, locations

    result: list[dict] = []
    seen: set[str] = set()
    for key in order:
        entry = by_key.get(key)
        if entry is not None and key not in seen:
            seen.add(key)
            result.append(entry)
    for key, entry in by_key.items():
        if key not in seen:
            result.append(entry)
    return result, normalize_relationships(relationships), locations


# ---------------------------------------------------------------------------
# coerce_model_update（TS: 656-703）
# ---------------------------------------------------------------------------

def coerce_model_update(
    raw: dict,
    previous: dict,
    book: dict,
    chapter: dict,
    merges: object = None,
    now_ms: int | None = None,
) -> dict:
    raw_memory = raw.get("memory") if is_record(raw.get("memory")) else raw
    # merges 正常由工具层从 args 提取后单独传入；直接嵌在 raw 里时兜底读取
    if merges is None:
        merges = raw.get("merges")

    worldview = normalize_worldview(merge_incremental_items(previous.get("worldview"), raw_memory.get("worldview")))
    characters = normalize_characters(merge_incremental_items(previous.get("characters"), raw_memory.get("characters")))
    relationships = normalize_relationships(merge_incremental_items(previous.get("relationships"), raw_memory.get("relationships")))
    locations = normalize_locations(merge_incremental_items(previous.get("locations"), raw_memory.get("locations")))

    characters, relationships, locations = apply_merges(characters, relationships, locations, merges)

    map_prompt = read_string(raw, "mapPrompt")
    should_regenerate_map = should_accept_map_regeneration(
        requested=bool(read_boolean(raw, "shouldRegenerateMap") or read_boolean(raw, "mapDirty")),
        map_prompt=map_prompt,
        previous=previous,
        locations=locations,
    )

    previous_map = previous.get("map")
    memory: dict[str, Any] = {
        **previous,
        **(raw_memory if is_record(raw_memory) else {}),
        "bookUrl": book.get("bookUrl"),
        "bookName": book.get("name"),
        "author": book.get("author"),
        # 模型不能改开关、不能伪造地图
        "enabled": previous.get("enabled"),
        "processedChapterIndex": chapter.get("index"),
        "processedChapterTitle": chapter.get("title"),
        "updatedAt": now_ms if now_ms is not None else int(time.time() * 1000),
        "summary": normalize_summary(raw_memory.get("summary"), previous.get("summary")),
        "worldview": worldview,
        "characters": characters,
        "relationships": relationships,
        "locations": locations,
        "map": previous_map if previous_map is not None else None,
        "mapDirty": should_regenerate_map,
    }
    memory.pop("lastError", None)

    return {
        "memory": memory,
        "shouldRegenerateMap": should_regenerate_map,
        "mapPrompt": map_prompt if should_regenerate_map else None,
    }


def clean_memory(memory: dict) -> dict:
    """去掉值为 None 的键，对齐 TS JSON.stringify 丢弃 undefined 的输出形态。"""
    return {key: value for key, value in memory.items() if value is not None}
