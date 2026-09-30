"""协议数据模型（pydantic）。

memory 及其内部实体刻意保持为宽松 dict（与 TS 版处理 unknown record 的方式
一致）：存量资料来自 SQLite 的 JSON 列，字段可能缺失或带历史杂项，
用严格模型反而会在合并管线里丢信息。协议帧（任务/事件）才用 pydantic。
"""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field


class ModelEndpoint(BaseModel):
    baseUrl: str = ""
    apiKey: str = ""
    model: str = ""
    useFullUrl: bool = False

    def ready(self) -> bool:
        return bool(self.baseUrl.strip() and self.model.strip())


class ImageModelEndpoint(ModelEndpoint):
    imageSize: str = "1024x1024"


class JobModelConfig(BaseModel):
    text: ModelEndpoint = Field(default_factory=ModelEndpoint)
    image: ImageModelEndpoint = Field(default_factory=ImageModelEndpoint)


class BookRef(BaseModel):
    name: str = ""
    author: str = ""
    bookUrl: str = ""

    model_config = ConfigDict(extra="allow")


class ChapterRef(BaseModel):
    index: int = 0
    title: str = ""
    url: str = ""

    model_config = ConfigDict(extra="allow")


class JobRequest(BaseModel):
    """run 帧的 job 载荷。chapter/chapters 仅 chapter_update 需要（redraw_map 不带）。"""

    jobId: str
    kind: Literal["chapter_update", "redraw_map"]
    book: BookRef = Field(default_factory=BookRef)
    chapter: ChapterRef | None = None
    chapters: list[dict[str, Any]] | None = None
    memory: dict[str, Any] = Field(default_factory=dict)
    model: JobModelConfig = Field(default_factory=JobModelConfig)

    model_config = ConfigDict(extra="allow")
