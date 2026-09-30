"""世界观条目的「章节复述」防线。

移植自 aiBookGeneration.ts:808-886。四级判定 + 兜底叙事动词计数，
词表逐字保留——这套词表是对真实模型输出（把本章剧情写进设定集）的对抗规则。
"""

from __future__ import annotations

import re

from .normalize import normalize_key

_TITLE_CHAPTER_RE = re.compile(r"(本章|章节|剧情|简介|概要|经过|第\d+章|第[一二三四五六七八九十百千万]+章)")
_CONTENT_CHAPTER_START_RE = re.compile(r"^(本章|本节|这一章|此章|第.+章)")
_CHAPTER_REFERENCE_RE = re.compile(r"第\s*(?:\d+|[零〇一二两三四五六七八九十百千万]+)\s*[章节回话卷篇]")

SUMMARY_CATEGORY_TERMS = ["当前事件", "章节摘要", "剧情进展", "本章剧情"]

PLOT_VERBS = ["搜查", "担心", "登上", "指出", "加入", "透露", "引出", "随后"]

NARRATIVE_TERMS = [
    "随后", "然后", "接着", "回到", "看到", "告诉", "解释", "拒绝", "催促", "等待",
    "躺在", "坐在", "担心", "考虑", "讲述", "寻找", "清晨", "下午", "晚上", "第二天",
]

SETTING_CATEGORY_TERMS = [
    "基础规则", "基础设定", "势力制度", "历史传说", "技术魔法",
    "社会文化", "地理环境", "组织体系", "未确认信息",
]


def is_chapter_summary_worldview(title: str, content: str, category: str) -> bool:
    category_key = normalize_key(category)
    content_key = normalize_key(content)
    if _TITLE_CHAPTER_RE.search(title):
        return True
    if any(normalize_key(term) in category_key for term in SUMMARY_CATEGORY_TERMS):
        return True
    if _CONTENT_CHAPTER_START_RE.match(content.strip()):
        return True
    if is_narrative_recap_text(title, content):
        return True
    plot_hits = sum(1 for term in PLOT_VERBS if normalize_key(term) in content_key)
    return len(content) > 80 and plot_hits >= 3 and not is_setting_category(category)


def is_narrative_recap_text(title: str, content: str) -> bool:
    trimmed = content.strip()
    combined = f"{title} {trimmed}"
    normalized = normalize_key(combined)
    sentence_count = len([p for p in re.split(r"[。！？；]", trimmed) if p.strip()])
    chapter_reference = bool(_CHAPTER_REFERENCE_RE.search(combined)) or any(
        normalize_key(term) in normalized for term in ["本章", "这一章", "当前章节", "章节内容"]
    )
    narrative_hits = sum(1 for term in NARRATIVE_TERMS if normalize_key(term) in normalized)
    return (
        chapter_reference and len(trimmed) > 60 and narrative_hits >= 2
    ) or (
        len(trimmed) > 140 and sentence_count >= 4 and narrative_hits >= 4
    )


def is_setting_category(category: str) -> bool:
    key = normalize_key(category)
    return any(normalize_key(term) in key for term in SETTING_CATEGORY_TERMS)
