"""merge/coerce 管线：来自 aiBookGeneration.test.ts 的领域用例 + merges 新协议。"""

from agent_sidecar.merge import (
    coerce_model_update,
    normalize_characters,
    normalize_locations,
    normalize_worldview,
)

BOOK = {"name": "诡秘之主", "author": "爱潜水的乌贼", "bookUrl": "book-1"}
CHAPTER = {"index": 8, "title": "第九章"}


def _empty_memory() -> dict:
    return {
        "bookUrl": "book-1",
        "enabled": True,
        "updatedAt": 0,
        "worldview": [],
        "characters": [],
        "relationships": [],
        "locations": [],
    }


def test_normalizes_important_model_memory_and_removes_duplicate_relationships():
    raw = {
        "memory": {
            "summary": "克莱恩开始熟悉廷根。",
            "worldview": [
                {"title": "非凡力量", "content": "存在超凡能力但细节未明。", "confidence": "已知"},
                {
                    "category": "基础规则",
                    "title": "非凡力量",
                    "content": "存在超凡能力，来源仍未确认。",
                    "confidence": "推断",
                },
            ],
            "characters": [
                {"name": "克莱恩", "status": "正在适应新身份", "description": "主角", "importance": "high"},
                {
                    "name": "克莱恩",
                    "status": "正在适应新身份并调查线索",
                    "aliases": ["周明瑞"],
                    "importance": "high",
                },
                {"name": "路人店员", "status": "卖过面包", "importance": "low"},
            ],
            "relationships": [
                {
                    "source": "克莱恩",
                    "target": "梅丽莎",
                    "relation": "兄妹",
                    "description": "共同生活，互相关心。",
                    "importance": "high",
                },
                {
                    "source": "梅丽莎",
                    "target": "克莱恩",
                    "relation": "兄妹",
                    "description": "梅丽莎关心哥哥的异常。",
                    "importance": "high",
                },
                {
                    "source": "克莱恩",
                    "target": "路人店员",
                    "relation": "认识",
                    "description": "买过东西",
                    "importance": "low",
                },
            ],
            "locations": [
                {"name": "廷根市", "kind": "城市", "description": "北大陆城市。", "importance": "high"},
                {
                    "name": "莫雷蒂公寓",
                    "parentName": "廷根市",
                    "kind": "住宅",
                    "description": "莫雷蒂一家居住地。",
                    "relatedCharacters": ["克莱恩"],
                },
                {
                    "name": "莫雷蒂公寓",
                    "parentName": "廷根市",
                    "kind": "住宅",
                    "description": "包含书桌和卧室的两居室公寓。",
                    "relatedCharacters": ["梅丽莎"],
                },
            ],
        },
        "shouldRegenerateMap": False,
    }

    update = coerce_model_update(raw, _empty_memory(), BOOK, CHAPTER, now_ms=0)
    memory = update["memory"]

    assert [item["category"] for item in memory["worldview"]] == ["基础设定", "基础规则"]
    assert [item["name"] for item in memory["characters"]] == ["克莱恩"]
    assert memory["characters"][0]["aliases"] == ["周明瑞"]
    assert memory["characters"][0]["status"] == "正在适应新身份并调查线索"
    assert len(memory["relationships"]) == 1
    assert memory["relationships"][0]["source"] == "克莱恩"
    assert memory["relationships"][0]["target"] == "梅丽莎"
    assert memory["relationships"][0]["relation"] == "兄妹"
    assert len(memory["locations"]) == 2
    apartment = next(item for item in memory["locations"] if item["name"] == "莫雷蒂公寓")
    assert apartment["parentName"] == "廷根市"
    assert apartment["description"] == "包含书桌和卧室的两居室公寓。"
    assert apartment["relatedCharacters"] == ["克莱恩", "梅丽莎"]


def test_merges_incremental_model_memory_with_existing_book_memory():
    previous = {
        **_empty_memory(),
        "summary": "主角仍在旧村。",
        "worldview": [{"category": "基础设定", "title": "灵脉", "content": "灵脉会影响修行。", "confidence": "已知"}],
        "characters": [{"name": "林舟", "status": "停留在旧村", "location": "旧村", "importance": "high"}],
        "relationships": [
            {"source": "林舟", "target": "村长", "relation": "师徒", "description": "村长曾指导林舟。", "importance": "medium"}
        ],
        "locations": [{"name": "旧村", "kind": "村落", "description": "故事开始的村落。", "importance": "high"}],
    }
    raw = {
        "memory": {
            "summary": "第十章「北境」：林舟离开旧村，抵达北境。",
            "worldview": [
                {"category": "地理环境", "title": "北境", "content": "北境是寒冷边境区域，已出现新的线索。", "confidence": "已知"}
            ],
            "characters": [
                {"name": "林舟", "status": "已离开旧村", "location": "北境", "importance": "high"},
                {"name": "沈月", "status": "在北境提供帮助", "importance": "medium"},
            ],
            "relationships": [
                {"source": "林舟", "target": "沈月", "relation": "临时同伴", "description": "两人在北境同行。", "importance": "medium"}
            ],
            "locations": [{"name": "北境", "kind": "区域", "description": "寒冷边境。", "importance": "high"}],
        },
        "shouldRegenerateMap": False,
    }

    update = coerce_model_update(raw, previous, {"name": "山海旧事", "author": "佚名", "bookUrl": "book-1"}, {"index": 9, "title": "第十章"}, now_ms=0)
    memory = update["memory"]

    # 模型回传单章式开头 → 保留旧累计摘要
    assert memory["summary"] == "主角仍在旧村。"
    assert [item["title"] for item in memory["worldview"]] == ["灵脉", "北境"]
    assert [item["name"] for item in memory["characters"]] == ["林舟", "沈月"]
    lin = next(item for item in memory["characters"] if item["name"] == "林舟")
    assert lin["status"] == "已离开旧村"
    assert lin["location"] == "北境"
    assert [f"{item['source']}-{item['relation']}-{item['target']}" for item in memory["relationships"]] == [
        "林舟-师徒-村长",
        "林舟-临时同伴-沈月",
    ]
    assert [item["name"] for item in memory["locations"]] == ["旧村", "北境"]


def test_does_not_regenerate_map_without_location_changes():
    previous = {
        **_empty_memory(),
        "characters": [{"name": "林舟", "status": "调查中", "importance": "high"}],
        "locations": [{"name": "旧村", "kind": "村落", "description": "故事开始的村落。", "importance": "high"}],
        "map": {"imageUrl": "/assets/ai-maps/old-map.png", "prompt": "绘制旧村地图。", "updatedAt": 100, "sourceChapterIndex": 9},
    }
    raw = {
        "memory": {
            "summary": "角色关系发生变化，但地点没有变化。",
            "characters": [{"name": "林舟", "status": "继续调查", "importance": "high"}],
            "relationships": [
                {"source": "林舟", "target": "沈月", "relation": "同伴", "description": "关系更稳定。", "importance": "medium"}
            ],
            "locations": [{"name": "旧村", "kind": "村落", "description": "故事开始的村落。", "importance": "high"}],
        },
        "shouldRegenerateMap": True,
        "mapPrompt": "重新绘制旧村地图。",
    }

    update = coerce_model_update(raw, previous, {"name": "山海旧事", "author": "佚名", "bookUrl": "book-1"}, {"index": 10, "title": "第十一章"}, now_ms=0)
    assert update["shouldRegenerateMap"] is False
    assert update["mapPrompt"] is None
    assert update["memory"]["mapDirty"] is False
    assert update["memory"]["map"]["imageUrl"] == "/assets/ai-maps/old-map.png"


def test_regenerates_map_with_new_location_changes():
    previous = {
        **_empty_memory(),
        "locations": [{"name": "旧村", "kind": "村落", "description": "故事开始的村落。", "importance": "high"}],
        "map": {"imageUrl": "/assets/ai-maps/old-map.png", "prompt": "绘制旧村地图。", "updatedAt": 100, "sourceChapterIndex": 9},
    }
    raw = {
        "memory": {
            "summary": "主角发现北境。",
            "locations": [
                {"name": "旧村", "kind": "村落", "description": "故事开始的村落。", "importance": "high"},
                {"name": "北境", "kind": "区域", "description": "新出现的寒冷边境。", "importance": "high"},
            ],
        },
        "shouldRegenerateMap": True,
        "mapPrompt": "把旧村与北境画在同一张区域地图上。",
    }

    update = coerce_model_update(raw, previous, {"name": "山海旧事", "author": "佚名", "bookUrl": "book-1"}, {"index": 11, "title": "第十二章"}, now_ms=0)
    assert update["shouldRegenerateMap"] is True
    assert update["mapPrompt"] == "把旧村与北境画在同一张区域地图上。"
    assert update["memory"]["mapDirty"] is True


def test_model_cannot_change_enabled_or_inject_map():
    previous = {**_empty_memory(), "enabled": False}
    raw = {
        "memory": {"enabled": True, "summary": "主角抵达北境。", "map": {"imageUrl": "fake"}},
        "shouldRegenerateMap": False,
    }
    update = coerce_model_update(raw, previous, BOOK, CHAPTER, now_ms=0)
    assert update["memory"]["enabled"] is False
    assert update["memory"]["map"] is None


def test_merges_protocol_unifies_aliases_and_rewrites_references():
    previous = {**_empty_memory(), "processedChapterIndex": 7}
    raw = {
        "memory": {
            "summary": "林师兄与林青云实为一人。",
            "characters": [
                {"name": "林青云", "status": "宗门弟子", "importance": "high"},
                {"name": "林师兄", "status": "在藏书阁当值", "importance": "high"},
            ],
            "relationships": [
                {"source": "林师兄", "target": "长老", "relation": "师徒", "description": "长老亲传弟子。", "importance": "high"},
                {"source": "林青云", "target": "长老", "relation": "师徒", "description": "长老亲传弟子，早年入门。", "importance": "high"},
            ],
            "locations": [
                {"name": "藏书阁", "description": "宗门藏书之处。", "relatedCharacters": ["林师兄", "林青云"]}
            ],
        },
        "shouldRegenerateMap": False,
        "merges": [{"canonical": "林青云", "absorbs": ["林师兄"]}],
    }

    update = coerce_model_update(raw, previous, BOOK, CHAPTER, now_ms=0)
    memory = update["memory"]

    assert [item["name"] for item in memory["characters"]] == ["林青云"]
    lin = memory["characters"][0]
    assert "林师兄" in (lin["aliases"] or [])
    # 两条关系在改名后归并为一条，且引用以规范名表述
    assert len(memory["relationships"]) == 1
    assert memory["relationships"][0]["source"] == "林青云"
    assert memory["relationships"][0]["description"] == "长老亲传弟子，早年入门。"
    assert memory["locations"][0]["relatedCharacters"] == ["林青云"]


def test_merges_ignore_absorbs_that_do_not_exist():
    # 模型幻觉出的名字：不凭空创建条目，也不把幻觉名塞进 aliases
    previous = {**_empty_memory(), "characters": [{"name": "林青云", "status": "宗门弟子", "importance": "high"}]}
    raw = {
        "memory": {"summary": "本章没有新角色。"},
        "shouldRegenerateMap": False,
        "merges": [{"canonical": "林青云", "absorbs": ["不存在的甲", "不存在的乙"]}],
    }
    update = coerce_model_update(raw, previous, BOOK, CHAPTER, now_ms=0)
    characters = update["memory"]["characters"]
    assert [item["name"] for item in characters] == ["林青云"]
    assert characters[0].get("aliases") == []


def test_merges_skip_canonical_that_does_not_exist():
    previous = {**_empty_memory(), "characters": [{"name": "林青云", "status": "宗门弟子", "importance": "high"}]}
    raw = {
        "memory": {"summary": "本章没有新角色。"},
        "shouldRegenerateMap": False,
        "merges": [{"canonical": "幻觉人物", "absorbs": ["林青云"]}],
    }
    update = coerce_model_update(raw, previous, BOOK, CHAPTER, now_ms=0)
    assert [item["name"] for item in update["memory"]["characters"]] == ["林青云"]


def test_merges_truncates_absorbs_beyond_eight():
    # 9 个吸收目标只吸收前 8 个；20 个角色吸收 8 个（剩 12）不触发坍缩守卫
    characters = [
        {"name": f"角色{index}", "status": "存在", "importance": "high"} for index in range(20)
    ]
    previous = {**_empty_memory(), "characters": characters}
    raw = {
        "memory": {"summary": "多名角色实为一人。"},
        "shouldRegenerateMap": False,
        "merges": [
            {"canonical": "角色0", "absorbs": [f"角色{index}" for index in range(1, 10)]}
        ],
    }
    update = coerce_model_update(raw, previous, BOOK, CHAPTER, now_ms=0)
    names = [item["name"] for item in update["memory"]["characters"]]
    assert len(names) == 12
    assert "角色8" not in names, "前 8 个（角色1..角色8）应被吸收"
    assert "角色9" in names, "第 9 个超出上限，应保留"


def test_merges_reject_collapsing_merge():
    # 4 个角色一次吸收 3 个 → 数量减少超过一半，整条 merge 拒绝
    characters = [
        {"name": f"角色{index}", "status": "存在", "importance": "high"} for index in range(4)
    ]
    previous = {**_empty_memory(), "characters": characters}
    raw = {
        "memory": {"summary": "疑似全员同一人。"},
        "shouldRegenerateMap": False,
        "merges": [{"canonical": "角色0", "absorbs": ["角色1", "角色2", "角色3"]}],
    }
    update = coerce_model_update(raw, previous, BOOK, CHAPTER, now_ms=0)
    names = [item["name"] for item in update["memory"]["characters"]]
    assert len(names) == 4, "坍缩式 merge 应被拒绝"


def test_unknown_top_level_memory_keys_are_dropped():
    raw = {
        "memory": {
            "summary": "正常增量。",
            "timeline": [{"event": "模型试图新增的顶层结构"}],
            "evilKey": "任意注入值",
        },
        "shouldRegenerateMap": False,
    }
    update = coerce_model_update(raw, _empty_memory(), BOOK, CHAPTER, now_ms=0)
    memory = update["memory"]
    assert memory["summary"] == "正常增量。"
    assert "timeline" not in memory, "未知顶层键不应落库"
    assert "evilKey" not in memory, "未知顶层键不应落库"


def test_normalize_locations_keeps_low_importance_entries():
    locations = normalize_locations([{"name": "无名小店", "description": "路边小店。", "importance": "low"}])
    assert len(locations) == 1


def test_normalize_characters_status_fallback_chain():
    characters = normalize_characters([{"name": "无名者", "description": "神秘人"}])
    assert characters[0]["status"] == "神秘人"
    assert normalize_characters([{"name": "空状态"}])[0]["status"] == "状态未知"


def test_normalize_worldview_drops_low_importance_and_requires_fields():
    notes = normalize_worldview(
        [
            {"title": "只有标题"},
            {"content": "只有内容"},
            {"title": "路人设定", "content": "不重要条目。", "importance": "low"},
            {"title": "有效条目", "content": "内容充分。", "category": "基础规则"},
        ]
    )
    assert [item["title"] for item in notes] == ["有效条目"]
