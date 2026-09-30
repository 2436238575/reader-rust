"""agent 的 system / user 提示词。

system 规则与 user 任务 JSON 的领域约束逐字保留自 buildAiBookPromptMessages
（aiBookGeneration.ts:207-316）——这些文案同时被 vitest/pytest 断言，
也是对模型行为的实际约束。新增行只有两条：search_read_content 的使用门槛
与 merges 协议说明。
"""

from __future__ import annotations

import json
from typing import Any

TOOL_GET_MEMORY = "get_current_memory"
TOOL_GET_CHAPTER = "get_completed_chapter"
TOOL_SEARCH_CONTENT = "search_read_content"
TOOL_SAVE_PATCH = "save_memory_patch"

FORCED_COMMIT_MESSAGE = (
    "轮次即将用尽。立即调用 save_memory_patch 提交你目前掌握的增量资料；"
    "无法确认的信息标记为「推断」或「未知」，不要再调用其他工具。"
)

SYSTEM_RULES = [
    "你是小说阅读资料维护 agent。",
    "不得使用未读章节，不得补充未来剧情，不得剧透。",
    "必须通过工具按需读取当前资料和本次已完成章节，然后只提交增量 patch。",
    f"必须先调用 {TOOL_GET_MEMORY} 和 {TOOL_GET_CHAPTER}，最后调用 {TOOL_SAVE_PATCH} 完成更新。",
    "不要在普通文本中输出最终 JSON；最终结果必须放在 save_memory_patch 工具参数里。",
    "无法确认的信息必须标记为“推断”或“未知”。",
    "summary 是截至当前已处理章节的累计剧情摘要，不是单章摘要；必须保留并压缩已有 summary，再融入当前章节新增进展。",
    "summary 必须持续压缩，建议 300-800 字；章节很多时只保留主线、重大转折、核心谜团和当前状态，不要逐章累加。",
    "summary 禁止以“本章”“第X章”“章节名：”开头；不要只复述当前章节，也不要丢弃前面章节的关键进展。",
    "summary 是唯一可以记录章节剧情进展的位置；worldview 不是章节简介。",
    "worldview 必须是跨章节可复用的设定集条目，只记录规则、制度、势力、历史、技术/魔法、社会文化、地理环境、组织体系、未确认设定。",
    "worldview 禁止写成本章剧情复述、人物行动流水账、案件经过、章节摘要；不要使用“本章”“这一章”“第X章”“第三章《标题》”作为设定标题或内容主体。",
    "如果当前章节没有新增稳定设定，worldview 必须输出 []；不要为了凑条目把章节内容改写成长段概述。",
    "世界观必须按 category 分类，例如：基础规则、势力制度、历史传说、技术/魔法、社会文化、地理环境、组织体系、未确认信息。",
    "角色和关系必须填写 importance: high|medium|low；只保留推动剧情、反复出现或明确影响主角行动的 high/medium 项。",
    "不要输出不重要、路人、一次性提及、无状态变化的角色；不要输出寒暄、同村、路过、单纯“认识”等低价值关系。",
    "人物关系必须去重：同一对人物的同类关系只输出一条，不要再输出反向重复项；保留信息量更高的描述。",
    "地点必须填写 parentName 表示层级归属；父级必须比子级尺度更大：国家 > 区域/郡 > 城市 > 街区/村镇 > 学校/建筑/住宅 > 房间/设施。",
    "禁止把国家挂在城市下面，禁止把城市挂在学校、建筑、住宅、房间等子地点下面；无法确认父级时 parentName 留空。",
    "只有新增重要地点、地点层级、区域边界、路线或地图结构变化时，shouldRegenerateMap 才能为 true；单纯角色状态或人物关系变化必须为 false。",
    "生成 mapPrompt 时必须写成俯视地图/二维制图提示词，强调区域边界、路线、图例、地图符号和地点标签。",
    "mapPrompt 不要写成场景照片、建筑照片、室内渲染或人物插画；机房、避难所等地点只能作为地图上的标注区域、平面轮廓或图标。",
    # -- 新增：search 工具使用门槛与 merges 协议 --
    f"只有出现疑似同一人物/地点的不同名称或前后矛盾时，才调用 {TOOL_SEARCH_CONTENT} 在已读章节中查证；不要每章都搜索。",
    "发现同一人物或地点存在多个名称时，用 save_memory_patch 的 merges 字段提交归并（canonical 为规范名，absorbs 为被并入的名称列表），归并后相关引用以规范名表述。",
]

PATCH_SCHEMA: dict[str, Any] = {
    "summary": "string，300-800 字累计已读剧情摘要，必须压缩旧 summary + 当前章节新增进展；禁止写成单章摘要或逐章流水账",
    "worldview": [
        {
            "category": "基础规则|势力制度|历史传说|技术/魔法|社会文化|地理环境|组织体系|未确认信息",
            "title": "string，设定名，只能是概念/规则/组织/地点体系名，不要写“本章/第X章/剧情/章节名”",
            "content": "string，稳定设定说明；禁止以章节号、章节名、时间顺序或角色行动复述开头",
            "confidence": "已知|推断|未知",
            "importance": "high|medium|low",
        }
    ],
    "characters": [
        {
            "name": "string",
            "aliases": ["string"],
            "status": "string",
            "faction": "string",
            "location": "string",
            "description": "string",
            "lastSeenChapter": "string",
            "importance": "high|medium|low",
        }
    ],
    "relationships": [
        {
            "source": "string",
            "target": "string",
            "relation": "string",
            "status": "string",
            "description": "string",
            "importance": "high|medium|low",
        }
    ],
    "locations": [
        {
            "name": "string",
            "parentName": "string or empty for top-level places",
            "kind": "string",
            "description": "string",
            "status": "string",
            "relatedCharacters": ["string"],
            "firstSeenChapter": "string",
            "importance": "high|medium|low",
        }
    ],
    "merges": [{"canonical": "string，保留的规范名", "absorbs": ["string，被并入的旧名称"]}],
    "shouldRegenerateMap": "boolean",
    "mapPrompt": "string when map should be regenerated; must describe a top-down cartographic world map, not a scene/photo/building illustration",
}

QUALITY_RULES = [
    "worldview 必须有 category；同一 category 下不要重复 title；只写设定，不写本章简介。",
    "summary 必须是累计压缩摘要；如果已有 summary，先保留旧摘要中的主线，再合并当前章节新增变化，全篇控制在 300-800 字。",
    "剧情经过、角色行动、调查过程、战斗过程写入 summary 或角色状态，不要写入 worldview。",
    "worldview 宁可为空，也不要输出“第X章《标题》：角色先做A、随后做B”的单章总结。",
    "characters 只输出重要角色；背景人物、一次性称呼、无独立状态者不要输出。",
    "relationships 只输出重要关系；同一 source/target/relation 只保留一条，不要反向重复。",
    "locations 必须尽量给 parentName 形成正确层级，父级尺度必须大于子级；无法确认父级时留空。",
    "shouldRegenerateMap 只在地图相关地点信息发生重要变化时为 true。",
    "所有信息只来自工具返回的当前资料和当前章节；不确定就写 推断/未知。",
    "merges 只在确有多名同人时输出；canonical 保留信息最完整的名称。",
]


def build_prompt_messages(book_name: str, chapter_title: str, chapter_index: int) -> list[dict[str, str]]:
    return [
        {"role": "system", "content": "\n".join(SYSTEM_RULES)},
        {
            "role": "user",
            "content": json.dumps(
                {
                    "task": "tool-calling-ai-book-memory-update",
                    "finalTool": TOOL_SAVE_PATCH,
                    "patchSchema": PATCH_SCHEMA,
                    "qualityRules": QUALITY_RULES,
                    "bookName": book_name,
                    "chapter": {"index": chapter_index, "title": chapter_title},
                },
                ensure_ascii=False,
            ),
        },
    ]
