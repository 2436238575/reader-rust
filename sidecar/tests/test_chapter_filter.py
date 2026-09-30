"""aiBookChapterFilter.test.ts 3 例的 1:1 移植 + 窗口化（非零起始）防回归。"""

from agent_sidecar.chapter_filter import (
    extract_chapter_ordinal,
    parse_chinese_number,
    should_skip_ai_book_chapter,
)

CHAPTERS = [
    {"index": 0, "title": "12月1日《诡秘之主》新番外发布"},
    {"index": 1, "title": "1417.新书已发"},
    {"index": 2, "title": "1416.不是诈尸，不是遭了阿蒙"},
    {"index": 3, "title": "1415.一个普通人的日常（八）"},
    {"index": 4, "title": "1414.一个普通人的日常（七）"},
    {"index": 5, "title": "1.第1章 绯红"},
    {"index": 6, "title": "2.第2章 情况"},
]


def test_skips_source_prefixed_latest_chapters_before_the_real_first_chapter():
    assert should_skip_ai_book_chapter(CHAPTERS[0], CHAPTERS) is True
    assert should_skip_ai_book_chapter(CHAPTERS[1], CHAPTERS) is True
    assert should_skip_ai_book_chapter(CHAPTERS[4], CHAPTERS) is True
    assert should_skip_ai_book_chapter(CHAPTERS[5], CHAPTERS) is False


def test_skips_non_story_extras_and_announcements():
    assert should_skip_ai_book_chapter({"index": 22, "title": "番外：普通人的日常"}) is True
    assert should_skip_ai_book_chapter({"index": 23, "title": "上架感言"}) is True
    assert should_skip_ai_book_chapter({"index": 24, "title": "新书已发，求支持"}) is True


def test_keeps_prologue_like_story_chapters():
    assert (
        should_skip_ai_book_chapter(
            {"index": 0, "title": "序章"},
            [{"index": 0, "title": "序章"}, {"index": 1, "title": "第一章 风起"}],
        )
        is False
    )


def test_windowed_nonzero_start_uses_real_index_positions():
    """窗口数组（真实 index 从 10 起）下，后 20 章前瞻按数组位置取。

    当前章 index=12（数组位置 2），低序号章在 index=15（数组位置 5）。
    按位置切片 slice(3,23) 能命中；按 TS 旧写法 slice(index+1=13, 33)
    会落到窗口位置 13 起，完全错过 index 15 —— 该用例区分两种实现。
    """
    window = [
        {"index": index, "title": f"{index + 60}.第{index + 60}章 日常"} for index in range(10, 40)
    ]
    window[5]["title"] = "3.第三章 插叙"
    current = {"index": 12, "title": "72.第七十二章"}
    assert should_skip_ai_book_chapter(current, window) is True

    # 反向对照：低序号章在窗口位置切片范围之外（index=15 → 改为 index=35，位置 25）
    window2 = [
        {"index": index, "title": f"{index + 60}.第{index + 60}章 日常"} for index in range(10, 40)
    ]
    window2[25]["title"] = "3.第三章 插叙"
    assert should_skip_ai_book_chapter(current, window2) is False


def test_first_main_index_compares_real_index_in_window():
    """窗口内第一个正文章（序号 1）的真实 index 参与比较。"""
    window = [
        {"index": 10, "title": "公告：作者的话"},
        {"index": 11, "title": "1.第1章 起步"},
        {"index": 12, "title": "55.第五十五章 最新"},
        {"index": 13, "title": "54.第五十四章 最新"},
    ]
    # 当前章 index=12 在第一个正文章（index=11）之后 → 不属于「最新章前置」
    assert should_skip_ai_book_chapter(window[2], window) is False
    # 若当前章排在第一个正文章之前（真实 index 语义）→ 跳过
    before_main = {"index": 10, "title": "60.第六十章 最新"}
    # 注意：index=10 这条的标题序号 60 且 index≤30；它排在 index=11 的正文章之前
    assert should_skip_ai_book_chapter(before_main, window) is True


def test_extract_chapter_ordinal_and_chinese_numbers():
    assert extract_chapter_ordinal("第一章") == 1
    assert extract_chapter_ordinal("第十二回") == 12
    assert extract_chapter_ordinal("第三百二十一章 风起") == 321
    assert extract_chapter_ordinal("1417.新书已发") == 1417
    assert parse_chinese_number("两千") == 2000
    assert parse_chinese_number("十二") == 12
    # 与 TS 一致：无单位的数字串直接累加末位（String.fromCharCode 式语义保持）
    assert parse_chinese_number("三零五") == 5
