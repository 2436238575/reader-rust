"""agent 轮次循环。

与旧前端版的实质差异：
- MAX_ROUNDS=5，末轮（或输入预算触顶）前注入强制提交消息——软着陆替代
  旧的「超过工具调用轮次限制」硬失败；
- 章节跳过过滤在取文之前执行，跳过的章不发起任何模型调用；
- 每轮结束后发 round 事件（观测用）。
"""

from __future__ import annotations

import json
import logging
from typing import Any

from .chapter_filter import should_skip_ai_book_chapter
from .json_extract import parse_json_content
from .map_prompt import build_fallback_map_prompt, request_map_image
from .merge import coerce_model_update, clean_memory
from .model_client import ModelClient, ModelError
from .prompts import FORCED_COMMIT_MESSAGE, build_prompt_messages
from .protocol import StdioChannel
from .tools import AGENT_TOOLS, ToolContext, execute_tool_call
from .types import JobRequest

log = logging.getLogger(__name__)

MAX_ROUNDS = 5
MAX_INPUT_CHARS = 300_000


def run_job(job: JobRequest, channel: StdioChannel | None, client: ModelClient) -> dict[str, Any]:
    if job.kind == "redraw_map":
        return _run_redraw_map(job, client)
    return _run_chapter_update(job, channel, client)


# ---------------------------------------------------------------------------
# chapter_update
# ---------------------------------------------------------------------------

def _run_chapter_update(
    job: JobRequest, channel: StdioChannel | None, client: ModelClient
) -> dict[str, Any]:
    chapter = job.chapter
    if chapter is None:
        raise ModelError("chapter_update 任务缺少 chapter 字段")

    chapters = job.chapters or []
    if should_skip_ai_book_chapter(
        {"index": chapter.index, "title": chapter.title}, chapters
    ):
        return {"skipped": True}

    if not job.model.text.ready():
        raise ModelError("文本模型未配置（baseUrl/model 为空）")

    if channel is not None:
        channel.send({"type": "status", "phase": "text", "message": f"更新 {chapter.title} 的 AI 资料..."})

    ctx = ToolContext(
        book=job.book,
        chapter=chapter,
        chapters=chapters,
        memory=job.memory,
        channel=channel,
        debug_chapter_content=_debug_chapter_content(job),
    )
    book_dict = job.book.model_dump()
    chapter_dict = {"index": chapter.index, "title": chapter.title}

    messages = build_prompt_messages(job.book.name, chapter.title, chapter.index)
    input_chars = 0
    forced_injected = False

    for step in range(MAX_ROUNDS):
        budget_exhausted = input_chars >= MAX_INPUT_CHARS
        is_final_round = step == MAX_ROUNDS - 1
        if (is_final_round or budget_exhausted) and not forced_injected:
            messages.append({"role": "user", "content": FORCED_COMMIT_MESSAGE})
            forced_injected = True

        payload = {
            "model": job.model.text.model,
            "messages": messages,
            "tools": AGENT_TOOLS,
            "tool_choice": "auto",
            "temperature": 0.2,
        }
        input_chars += len(json.dumps(payload, ensure_ascii=False))

        data = client.chat(job.model.text, payload)
        message = _first_message(data)
        tool_calls = message.get("tool_calls") if isinstance(message.get("tool_calls"), list) else []
        if channel is not None:
            channel.send(
                {
                    "type": "round",
                    "step": step + 1,
                    "toolCalls": [
                        str((tc.get("function") or {}).get("name") or "") for tc in tool_calls
                    ],
                }
            )

        if tool_calls:
            messages.append(
                {
                    "role": "assistant",
                    "content": message.get("content"),
                    "tool_calls": tool_calls,
                }
            )
            for tool_call in tool_calls:
                outcome = execute_tool_call(tool_call, ctx)
                messages.append(
                    {
                        "role": "tool",
                        "tool_call_id": str(tool_call.get("id") or ""),
                        "name": str((tool_call.get("function") or {}).get("name") or ""),
                        "content": json.dumps(outcome.content, ensure_ascii=False),
                    }
                )
                if outcome.final and outcome.raw is not None:
                    update = coerce_model_update(
                        outcome.raw,
                        job.memory,
                        book_dict,
                        chapter_dict,
                        merges=outcome.merges,
                    )
                    return _chapter_result(update)
            continue

        content = message.get("content")
        if content:
            raw = parse_json_content(str(content))
            update = coerce_model_update(raw, job.memory, book_dict, chapter_dict)
            return _chapter_result(update)

    raise ModelError("AI 资料生成超过工具调用轮次限制")


def _chapter_result(update: dict[str, Any]) -> dict[str, Any]:
    return {
        "memory": clean_memory(update["memory"]),
        "shouldRegenerateMap": update["shouldRegenerateMap"],
        "mapPrompt": update.get("mapPrompt"),
    }


def _first_message(data: dict) -> dict[str, Any]:
    choices = data.get("choices") if isinstance(data, dict) else None
    if isinstance(choices, list) and choices and isinstance(choices[0], dict):
        message = choices[0].get("message")
        if isinstance(message, dict):
            return message
    return {}


def _debug_chapter_content(job: JobRequest) -> str | None:
    extra = job.model_extra or {}
    value = extra.get("chapterContent")
    return value if isinstance(value, str) else None


# ---------------------------------------------------------------------------
# redraw_map
# ---------------------------------------------------------------------------

def _run_redraw_map(job: JobRequest, client: ModelClient) -> dict[str, Any]:
    memory = job.memory
    map_data = memory.get("map") if isinstance(memory.get("map"), dict) else {}
    prompt = str(map_data.get("prompt") or "").strip() or build_fallback_map_prompt(memory, job.book.model_dump())

    image = request_map_image(client, job.model.image, prompt)
    return {
        "map": image,
        "mapPrompt": prompt,
        "sourceChapterIndex": memory.get("processedChapterIndex"),
    }
