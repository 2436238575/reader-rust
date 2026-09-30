"""世界地图：提示词包装、兜底提示词、图片模型调用。

build_map_image_prompt 逐字移植自 aiBookGeneration.ts:1104-1116（制图约束
模板是对「把地图画成场景照片」的对抗规则）；build_fallback_map_prompt 移植
自 stores/aiBook.ts:308-321（图片模型生成失败时的关系图兜底也用它作提示词）。
"""

from __future__ import annotations

from .model_client import ModelClient, ModelError
from .types import ImageModelEndpoint


def build_map_image_prompt(prompt: str) -> str:
    source_prompt = prompt.strip() or "根据已读进度中的已知地点绘制小说世界地图。"
    return "\n".join(
        [
            "请生成一张小说世界地图，而不是场景插画。",
            "画面类型：俯视地图（top-down / orthographic map）、二维制图、设定集地图。",
            "必须表现：区域边界、道路或虚线连接、地形/空间分区、地图符号、地点标签、图例、罗盘或比例尺感。",
            "地点呈现方式：机房、避难所等室内或建筑地点只能表现为地图上的标注区域、平面轮廓或小图标。",
            "禁止内容：不要生成写实照片、电影截图、建筑外观特写、室内房间透视图、服务器机柜照片、避难所入口照片。",
            "不要画人物，不要把地点画成可进入的真实建筑场景，不要用巨大门牌或数字替代地图标注。",
            "构图要求：清晰分区，路线关系可读，整体像游戏世界地图、桌面 RPG 区域地图或小说设定集地图。",
            f"原始地图信息：{source_prompt}",
        ]
    )


def build_fallback_map_prompt(memory: dict, book: dict) -> str:
    lines = []
    for location in memory.get("locations") or []:
        if not isinstance(location, dict):
            continue
        name = str(location.get("name") or "")
        if not name:
            continue
        parent = location.get("parentName")
        kind = location.get("kind")
        prefix = f"{parent} > " if parent else ""
        suffix = f"（{kind}）" if kind else ""
        lines.append(f"{prefix}{name}{suffix}: {location.get('description') or ''}")
    locations_text = "\n".join(lines)
    book_name = str(book.get("name") or "")
    return "\n".join(
        [
            f"为小说《{book_name}》绘制一张不剧透的世界地图。",
            "只包含已读进度中出现的地点和势力范围。",
            "优先表现地点层级、区域边界、路线连接、图例和地点标签，避免画成建筑外观或场景照片。",
            locations_text or memory.get("summary") or "保留未知区域，以卷轴地图风格呈现。",
        ]
    )


def request_map_image(client: ModelClient, endpoint: ImageModelEndpoint, prompt: str) -> dict:
    """调用图片模型，返回 {b64Json?, imageUrl?}。失败抛 ModelError（Rust 据此走关系图兜底）。"""
    if not endpoint.ready():
        raise ModelError("图片模型未配置")
    data = client.images(
        endpoint,
        {
            "model": endpoint.model,
            "prompt": build_map_image_prompt(prompt),
            "size": endpoint.imageSize or "1024x1024",
            "response_format": "b64_json",
            "n": 1,
        },
    )
    items = data.get("data") if isinstance(data, dict) else None
    first = items[0] if isinstance(items, list) and items and isinstance(items[0], dict) else None
    b64 = first.get("b64_json") if first else None
    image_url = first.get("url") if first else None
    if not b64 and not image_url:
        raise ModelError("地图生成结果为空")
    return {"b64Json": b64, "imageUrl": image_url}
