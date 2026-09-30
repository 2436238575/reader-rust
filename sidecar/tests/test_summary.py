"""summary 规则：来自 aiBookGeneration.test.ts 的累计摘要用例 + 拒单章开头。"""

from agent_sidecar.summary import (
    limit_summary_length,
    normalize_summary,
    starts_with_single_chapter_summary,
)

LONG_SUMMARY = "主线开端。" + "主角持续调查线索，局势逐步升级。" * 120 + "当前进展仍集中在北境。"


def test_keeps_cumulative_summaries_bounded_instead_of_growing_by_chapter():
    result = limit_summary_length(LONG_SUMMARY)
    assert len(result) == 1200
    assert "……" in result
    assert result.startswith("主线开端。")
    assert result.endswith("当前进展仍集中在北境。")


def test_rejects_single_chapter_summary_and_keeps_previous():
    assert normalize_summary("第十章「北境」：林舟离开旧村，抵达北境。", "主角仍在旧村。") == "主角仍在旧村。"
    assert normalize_summary("本章：主角抵达北境。", "主角仍在旧村。") == "主角仍在旧村。"


def test_strips_single_chapter_heading_when_previous_empty():
    assert normalize_summary("本章：主角抵达北境。", "") == "主角抵达北境。"
    assert normalize_summary("第十章「北境」：林舟抵达北境。", None) == "林舟抵达北境。"


def test_non_string_next_falls_back_to_previous():
    assert normalize_summary(None, "主角仍在旧村。") == "主角仍在旧村。"
    assert normalize_summary(123, "主角仍在旧村。") == "主角仍在旧村。"


def test_starts_with_single_chapter_summary_patterns():
    assert starts_with_single_chapter_summary("本章，主角……") is True
    assert starts_with_single_chapter_summary("第一千章：标题") is True
    assert starts_with_single_chapter_summary("主角抵达北境。") is False
