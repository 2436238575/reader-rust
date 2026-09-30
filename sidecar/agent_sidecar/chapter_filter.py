"""章节跳过过滤（非正文章 / 最新章前置识别）。

移植自 aiBookChapterFilter.ts。与 TS 版有一处刻意差异（窗口化适配）：
Rust 下发的 chapters 是窗口切片而非全量目录，条目携带真实 index；
TS 版两处把 `chapter.index` 直接当数组下标用（findIndex 的比较与
`chapters.slice(chapter.index + 1, ...)`），这里统一改为
「先按真实 index 定位当前章在窗口数组中的位置，再取后 20 条」，
firstMainIndex 比较也改为真实 index 语义。全量目录输入下行为与 TS 一致。
"""

from __future__ import annotations

import re
from typing import Any

NON_STORY_TITLES = [
    "番外", "外传", "新书", "新番外", "公告", "通知", "说明", "感言", "请假",
    "断更", "恢复更新", "更新时间", "求票", "月票", "推荐票", "打赏", "推书",
    "书友群", "群号", "活动", "抽奖", "实体书", "出版", "完本", "上架",
]

PROLOGUE_PATTERNS = [r"^序章$", r"^楔子$", r"^引子$", r"^序$", r"^前言$"]

_ARABIC_ORDINAL_RE = re.compile(r"^(?:第)?\s*(\d{1,5})\s*(?:[章节回话集部卷篇\.、:：\s]|$)")
_CHINESE_ORDINAL_RE = re.compile(r"^第?\s*([零〇一二两三四五六七八九十百千万]+)\s*(?:章|节|回|话|集|部|卷|篇)")

_CHINESE_DIGITS = {
    "零": 0, "〇": 0, "一": 1, "二": 2, "两": 2, "三": 3, "四": 4,
    "五": 5, "六": 6, "七": 7, "八": 8, "九": 9,
}
_CHINESE_UNITS = {"十": 10, "百": 100, "千": 1000, "万": 10000}


def should_skip_ai_book_chapter(chapter: dict[str, Any], chapters: list[dict[str, Any]] | None = None) -> bool:
    title = normalize_chapter_title(str(chapter.get("title") or ""))
    if not title:
        return True
    if any(re.fullmatch(pattern, title) for pattern in PROLOGUE_PATTERNS):
        return False
    if any(term in title for term in NON_STORY_TITLES):
        return True
    return is_front_loaded_latest_chapter(chapter, chapters or [])


def is_front_loaded_latest_chapter(chapter: dict[str, Any], chapters: list[dict[str, Any]]) -> bool:
    """识别「书源把最新章节排到最前面」的情况，只对前 31 章内生效。"""
    chapter_index = int(chapter.get("index") or 0)
    if not chapters or chapter_index > 30:
        return False

    ordinal = extract_chapter_ordinal(str(chapter.get("title") or ""))
    if not ordinal or ordinal < 50:
        return False

    first_main_index: int | None = None
    for item in chapters:
        item_title = normalize_chapter_title(str(item.get("title") or ""))
        if any(re.fullmatch(pattern, item_title) for pattern in PROLOGUE_PATTERNS):
            continue
        if extract_chapter_ordinal(str(item.get("title") or "")) == 1:
            first_main_index = int(item.get("index") or 0)
            break

    if first_main_index is not None and chapter_index < first_main_index:
        return True

    pos = _position_by_index(chapters, chapter_index)
    if pos is None:
        return False
    # TS: chapters.slice(chapter.index + 1, min(len, chapter.index + 20)) —— 19 条（不含 end）
    for item in chapters[pos + 1 : pos + 20]:
        next_ordinal = extract_chapter_ordinal(str(item.get("title") or ""))
        if isinstance(next_ordinal, int) and next_ordinal <= 10:
            return True
    return False


def _position_by_index(chapters: list[dict[str, Any]], chapter_index: int) -> int | None:
    for position, item in enumerate(chapters):
        try:
            if int(item.get("index") or 0) == chapter_index:
                return position
        except (TypeError, ValueError):
            continue
    return None


def extract_chapter_ordinal(title: str) -> int | None:
    normalized = normalize_chapter_title(title)
    if not normalized:
        return None

    arabic = _ARABIC_ORDINAL_RE.match(normalized)
    if arabic:
        return int(arabic.group(1))

    chinese = _CHINESE_ORDINAL_RE.match(normalized)
    if chinese:
        return parse_chinese_number(chinese.group(1))

    return None


def normalize_chapter_title(title: str) -> str:
    text = re.sub(r"\s+", "", str(title or ""))
    text = re.sub(r"[【】《》（）()]", "", text)
    return text.strip()


def parse_chinese_number(value: str) -> int | None:
    total = 0
    section = 0
    number = 0
    for char in value:
        if char in _CHINESE_DIGITS:
            number = _CHINESE_DIGITS[char]
            continue
        unit = _CHINESE_UNITS.get(char)
        if not unit:
            return None
        if unit == 10000:
            section = (section + number) * unit
            total += section
            section = 0
        else:
            section += (number or 1) * unit
        number = 0
    return total + section + number
