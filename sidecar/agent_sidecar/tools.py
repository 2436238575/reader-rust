"""agent 工具定义与执行分发。

三个本地/回调工具 + 一个终结工具：
- get_current_memory：本地执行（memory 随 run 帧全量下发），支持 include 过滤；
- get_completed_chapter / search_read_content：回调工具，经 stdio 通道向 Rust
  取内容（统一 get_book_content 链路，覆盖本地书）；skip 过滤在取文之前执行，
  跳过的章零流量；
- save_memory_patch：终结工具，模型在这里提交增量 patch + merges。
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass, field
from typing import Any

from .merge import (
    is_record,
    normalize_characters,
    normalize_locations,
    normalize_relationships,
    normalize_worldview,
)
from .normalize import normalize_key
from .prompts import (
    TOOL_GET_CHAPTER,
    TOOL_GET_MEMORY,
    TOOL_SAVE_PATCH,
    TOOL_SEARCH_CONTENT,
)
from .protocol import StdioChannel
from .types import BookRef, ChapterRef

log = logging.getLogger(__name__)

DEFAULT_CHAPTER_WINDOW = 24000
MAX_SEARCH_SNIPPETS = 5
SEARCH_SNIPPET_CHARS = 240

AGENT_TOOLS = [
    {
        "type": "function",
        "function": {
            "name": TOOL_GET_MEMORY,
            "description": "读取当前已保存的小说 AI 资料。只返回已读进度内的结构化记忆。",
            "parameters": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["summary", "worldview", "characters", "relationships", "locations"],
                        "description": "只取某一类资料（默认全量）。",
                    },
                    "query": {
                        "type": "string",
                        "description": "按名称/关键词过滤条目（对 characters 等列表类生效）。",
                    },
                },
                "additionalProperties": False,
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": TOOL_GET_CHAPTER,
            "description": (
                "读取本次需要处理的已完成章节正文，不会返回未读章节。"
                "超长章节会被截断并标记 truncated，可用 offset 续读剩余部分。"
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "offset": {"type": "integer", "description": "起始字符偏移，默认 0。"},
                    "limit": {"type": "integer", "description": "本次读取的字符数上限。"},
                },
                "additionalProperties": False,
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": TOOL_SEARCH_CONTENT,
            "description": (
                "在已读章节正文中搜索关键词，返回命中片段。"
                "仅当出现疑似同一人物/地点的不同名称或前后矛盾时使用；不要每章都搜索。"
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "keyword": {"type": "string", "description": "要搜索的名称或关键词。"},
                    "fromIndex": {"type": "integer", "description": "起始章节 index（含）。"},
                    "toIndex": {"type": "integer", "description": "结束章节 index（含），不会超过当前已处理进度。"},
                },
                "required": ["keyword"],
                "additionalProperties": False,
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": TOOL_SAVE_PATCH,
            "description": "提交本章带来的结构化资料增量。必须在已读取当前资料和章节后调用一次作为最终结果。",
            "parameters": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "memory": {
                        "type": "object",
                        "additionalProperties": True,
                        "description": "增量资料，不要整包覆盖。可包含 summary、worldview、characters、relationships、locations。",
                    },
                    "merges": {
                        "type": "array",
                        "description": "同一实体多名时的归并操作：canonical 保留，absorbs 并入（会同步改写关系/地点里的引用）。",
                        "items": {
                            "type": "object",
                            "additionalProperties": False,
                            "properties": {
                                "canonical": {"type": "string"},
                                "absorbs": {"type": "array", "items": {"type": "string"}},
                            },
                            "required": ["canonical", "absorbs"],
                        },
                    },
                    "shouldRegenerateMap": {
                        "type": "boolean",
                        "description": "只有重要地点、层级、路线或地图结构变化时为 true。",
                    },
                    "mapPrompt": {
                        "type": "string",
                        "description": "需要重绘地图时的俯视二维制图提示词。",
                    },
                },
                "required": ["memory", "shouldRegenerateMap"],
            },
        },
    },
]


@dataclass
class ToolOutcome:
    content: dict[str, Any]
    final: bool = False
    raw: dict[str, Any] | None = None
    merges: list[Any] | None = None


@dataclass
class ToolContext:
    book: BookRef
    chapter: ChapterRef | None
    chapters: list[dict[str, Any]]
    memory: dict[str, Any]
    channel: StdioChannel | None
    debug_chapter_content: str | None = field(default=None)


def execute_tool_call(tool_call: dict[str, Any], ctx: ToolContext) -> ToolOutcome:
    name = str((tool_call.get("function") or {}).get("name") or "")
    args_result = _parse_tool_arguments((tool_call.get("function") or {}).get("arguments") or "{}")
    if not args_result["ok"]:
        return ToolOutcome({"ok": False, "error": args_result["error"]})
    args = args_result["value"]

    if name == TOOL_GET_MEMORY:
        return ToolOutcome({"ok": True, "memory": build_agent_memory_context(ctx.memory, args)})

    if name == TOOL_GET_CHAPTER:
        return _tool_get_chapter(args, ctx)

    if name == TOOL_SEARCH_CONTENT:
        return _tool_search_content(args, ctx)

    if name == TOOL_SAVE_PATCH:
        # 旧版的 memoryPatch/patch 别名已按计划移除；模型若用了错误键名，
        # 显式报错让它重试，而不是把整个 args（含杂键）当资料落库
        if not is_record(args.get("memory")):
            return ToolOutcome(
                {
                    "ok": False,
                    "error": "save_memory_patch 必须在 memory 键下提交增量资料对象",
                }
            )
        memory = args.get("memory")
        merges = args.get("merges") if isinstance(args.get("merges"), list) else None
        raw = {
            "memory": memory,
            "shouldRegenerateMap": args.get("shouldRegenerateMap") is True or args.get("mapDirty") is True,
            "mapPrompt": args.get("mapPrompt") if isinstance(args.get("mapPrompt"), str) else "",
        }
        return ToolOutcome({"ok": True, "accepted": True}, final=True, raw=raw, merges=merges)

    return ToolOutcome({"ok": False, "error": f"未知工具：{name}"})


# ---------------------------------------------------------------------------
# 工具实现
# ---------------------------------------------------------------------------

_MEMORY_SECTIONS = ("summary", "worldview", "characters", "relationships", "locations")


def build_agent_memory_context(memory: dict, include: dict[str, Any] | None = None) -> dict[str, Any]:
    context: dict[str, Any] = {
        "bookUrl": memory.get("bookUrl"),
        "bookName": memory.get("bookName"),
        "author": memory.get("author"),
        "processedChapterIndex": memory.get("processedChapterIndex"),
        "processedChapterTitle": memory.get("processedChapterTitle"),
        "summary": memory.get("summary") or "",
        "worldview": normalize_worldview(_as_list(memory.get("worldview"))),
        "characters": normalize_characters(_as_list(memory.get("characters"))),
        "relationships": normalize_relationships(_as_list(memory.get("relationships"))),
        "locations": normalize_locations(_as_list(memory.get("locations"))),
        "map": _map_summary(memory),
        "mapDirty": bool(memory.get("mapDirty")),
    }

    if not is_record(include):
        return context
    kind = include.get("kind")
    if kind not in _MEMORY_SECTIONS:
        return context
    query = normalize_key(str(include.get("query") or ""))

    filtered: dict[str, Any] = {
        "bookUrl": context["bookUrl"],
        "bookName": context["bookName"],
        "processedChapterIndex": context["processedChapterIndex"],
    }
    if kind == "summary":
        filtered["summary"] = context["summary"]
        return filtered

    items = context[kind]
    if not query:
        filtered[kind] = items
        return filtered
    filtered[kind] = [item for item in items if _matches_query(kind, item, query)]
    return filtered


def _matches_query(kind: str, item: dict, query: str) -> bool:
    if kind == "worldview":
        fields = ("title", "category", "content")
    elif kind == "characters":
        fields = ("name", "faction", "location", "description")
    elif kind == "relationships":
        fields = ("source", "target", "relation", "description")
    else:
        fields = ("name", "parentName", "description")
    haystacks = [str(item.get(f) or "") for f in fields]
    if kind == "characters":
        haystacks += [str(a) for a in (item.get("aliases") or [])]
    return any(query in normalize_key(h) for h in haystacks)


def _map_summary(memory: dict) -> dict[str, Any] | None:
    map_data = memory.get("map")
    if not is_record(map_data):
        return None
    return {
        "prompt": map_data.get("prompt"),
        "sourceChapterIndex": map_data.get("sourceChapterIndex"),
        "fallback": map_data.get("fallback"),
        "fallbackReason": map_data.get("fallbackReason"),
    }


def _as_list(value: object) -> list:
    return value if isinstance(value, list) else []


def _tool_get_chapter(args: dict[str, Any], ctx: ToolContext) -> ToolOutcome:
    if ctx.chapter is None:
        return ToolOutcome({"ok": False, "error": "当前任务没有章节上下文"})

    content = _fetch_chapter_content(ctx)
    if isinstance(content, ToolOutcome):
        return content
    total = len(content)

    offset = _read_int(args, "offset", 0)
    limit = _read_int(args, "limit", DEFAULT_CHAPTER_WINDOW)
    offset = max(0, offset)
    limit = max(1, limit)
    window = content[offset : offset + limit]

    return ToolOutcome(
        {
            "ok": True,
            "book": {"name": ctx.book.name, "author": ctx.book.author, "bookUrl": ctx.book.bookUrl},
            "chapter": {"index": ctx.chapter.index, "title": ctx.chapter.title, "content": window},
            "offset": offset,
            "totalLength": total,
            "truncated": offset + limit < total,
        }
    )


def _fetch_chapter_content(ctx: ToolContext) -> str | ToolOutcome:
    """回调取文；调试模式（无宿主）可用 run 帧携带的 chapterContent。"""
    if ctx.channel is not None:
        try:
            response = ctx.channel.call_host_tool(
                TOOL_GET_CHAPTER, {"index": ctx.chapter.index if ctx.chapter else 0}
            )
        except Exception as error:  # noqa: BLE001 — 回调失败必须以工具结果返回给模型
            log.warning("get_completed_chapter 回调失败：%s", error)
            return ToolOutcome({"ok": False, "error": f"章节内容获取失败：{error}"})
        if not response.get("ok"):
            return ToolOutcome({"ok": False, "error": str(response.get("error") or "章节内容获取失败")})
        return str(response.get("content") or "")

    if ctx.debug_chapter_content is not None:
        return ctx.debug_chapter_content
    return ToolOutcome({"ok": False, "error": "调试模式缺少 chapterContent，无法提供章节正文"})


def _tool_search_content(args: dict[str, Any], ctx: ToolContext) -> ToolOutcome:
    keyword = str(args.get("keyword") or "").strip()
    if not keyword:
        return ToolOutcome({"ok": False, "error": "keyword 不能为空"})

    processed = ctx.memory.get("processedChapterIndex")
    processed_index = processed if isinstance(processed, int) else -1
    from_index = _read_int(args, "fromIndex", 0)
    to_index = _read_int(args, "toIndex", processed_index)
    from_index = max(0, from_index)
    # 不剧透钳制：搜索范围不得超过当前已处理进度（Rust 端点会再钳一次）
    to_index = min(to_index, processed_index)
    if from_index > to_index:
        return ToolOutcome({"ok": False, "error": f"搜索范围无效：fromIndex {from_index} 超出已读进度"})

    if ctx.channel is None:
        return ToolOutcome({"ok": False, "error": "调试模式不支持章节搜索"})

    try:
        response = ctx.channel.call_host_tool(
            TOOL_SEARCH_CONTENT,
            {"keyword": keyword, "fromIndex": from_index, "toIndex": to_index},
        )
    except Exception as error:  # noqa: BLE001
        log.warning("search_read_content 回调失败：%s", error)
        return ToolOutcome({"ok": False, "error": f"章节搜索失败：{error}"})
    if not response.get("ok"):
        return ToolOutcome({"ok": False, "error": str(response.get("error") or "章节搜索失败")})

    matches = response.get("matches")
    return ToolOutcome(
        {
            "ok": True,
            "keyword": keyword,
            "range": {"fromIndex": from_index, "toIndex": to_index},
            "matches": matches if isinstance(matches, list) else [],
        }
    )


def _read_int(args: dict[str, Any], key: str, default: int) -> int:
    value = args.get(key)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return default
    return int(value)


def _parse_tool_arguments(raw: object) -> dict[str, Any]:
    try:
        parsed = json.loads(raw if isinstance(raw, str) else "{}")
    except json.JSONDecodeError as error:
        return {"ok": False, "error": f"工具参数不是有效 JSON：{error}"}
    if not isinstance(parsed, dict):
        return {"ok": False, "error": "工具参数必须是 JSON 对象"}
    return {"ok": True, "value": parsed}
