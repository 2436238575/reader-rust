"""累计摘要的清洗与压缩规则。

移植自 aiBookGeneration.ts:712-750。核心语义：
- summary 是跨章累计摘要，模型回传单章式开头（「第十章「北境」：」）时拒绝采用；
- 上限 1200 字（按码点计），超长保留头 55% + 尾部余量，中间以「……」衔接。
"""

from __future__ import annotations

import re

MAX_AI_BOOK_SUMMARY_CHARS = 1200

_THIS_CHAPTER_PREFIX_RE = re.compile(r"^(?:本章|本节|这一章)[：:，,]")
_CHAPTER_HEADING_RE = re.compile(
    r"^第\s*(?:\d+|[零〇一二两三四五六七八九十百千万]+)\s*[章节回话卷篇][^。！？；]{0,40}[：:]"
)


def normalize_summary(next_summary: object, previous_summary: str | None) -> str:
    previous = (previous_summary or "").strip()
    if not isinstance(next_summary, str):
        return previous

    text = next_summary.strip()
    if not text:
        return limit_summary_length(previous)
    if not previous:
        return limit_summary_length(strip_single_chapter_summary_heading(text))
    if starts_with_single_chapter_summary(text):
        return limit_summary_length(previous)
    return limit_summary_length(text)


def starts_with_single_chapter_summary(value: str) -> bool:
    text = value.strip()
    return bool(
        _THIS_CHAPTER_PREFIX_RE.match(text)
        or _CHAPTER_HEADING_RE.match(text)
    )


def strip_single_chapter_summary_heading(value: str) -> str:
    text = value.strip()
    text = re.sub(r"^(?:本章|本节|这一章)[：:，,]\s*", "", text)
    text = re.sub(
        r"^第\s*(?:\d+|[零〇一二两三四五六七八九十百千万]+)\s*[章节回话卷篇][^。！？；]{0,40}[：:]\s*",
        "",
        text,
    )
    return text.strip()


def limit_summary_length(value: str) -> str:
    text = value.strip()
    chars = list(text)
    if len(chars) <= MAX_AI_BOOK_SUMMARY_CHARS:
        return text

    head_length = int((MAX_AI_BOOK_SUMMARY_CHARS - 2) * 0.55)
    tail_length = MAX_AI_BOOK_SUMMARY_CHARS - 2 - head_length
    return "".join(chars[:head_length]) + "……" + "".join(chars[-tail_length:])
