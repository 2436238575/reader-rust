"""aiBookNormalize.test.ts 8 例的 1:1 移植。"""

from agent_sidecar.normalize import (
    importance_rank,
    is_low_importance,
    is_low_value_relationship,
    normalize_key,
    prefer_importance,
    relationship_key,
    richer_string,
    unique_strings,
)


def test_normalize_key_忽略大小写空白中点变体():
    assert normalize_key(" 张三 ·Feng ") == "张三.feng"
    assert normalize_key(None) == ""
    assert normalize_key("") == ""


def test_is_low_importance_命中中英文低价值词():
    assert is_low_importance("路人") is True
    assert is_low_importance("Low") is True
    assert is_low_importance("重要角色") is False
    assert is_low_importance(None) is False


def test_importance_rank_四档():
    assert importance_rank("高") == 3
    assert importance_rank("medium") == 2
    assert importance_rank("背景") == 1
    assert importance_rank(None) == 0


def test_richer_string_取内容更充实者():
    assert richer_string(None, "a") == "a"
    assert richer_string("长描述", "短") == "长描述"
    assert richer_string("短", "更长的描述") == "更长的描述"


def test_prefer_importance_取评级更高者():
    assert prefer_importance("低", "高") == "高"
    assert prefer_importance("高", "低") == "高"


def test_unique_strings_按归一化键去重但保留首个原文():
    assert unique_strings(["张三", " 张三 ", "李四", ""]) == ["张三", "李四"]


def test_relationship_key_无向点对加关系词():
    assert relationship_key("甲", "乙", "朋友") == relationship_key("乙", "甲", "朋友")
    assert relationship_key("甲", "乙", "朋友") != relationship_key("甲", "乙", "敌人")


def test_is_low_value_relationship_只过滤弱关系且描述过短的边():
    assert is_low_value_relationship("认识", "见过一面", None) is True
    assert is_low_value_relationship("认识", "见过一面", "高") is False
    assert is_low_value_relationship("挚友", "x", None) is False
    assert (
        is_low_value_relationship("认识", "从第三章起共同行动并多次互相救援，结下深厚情谊", None)
        is False
    )
