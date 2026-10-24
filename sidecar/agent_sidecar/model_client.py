"""OpenAI 兼容模型客户端（同步 httpx）。

取代前端旧版的 fetch 三路径（直连 / 浏览器代理 / server 模式）：sidecar 从
Rust 拿到的就是解析后的 endpoint（baseUrl/apiKey/model/useFullUrl），
只剩「拼接 URL 并直连」一条路。新增 429/5xx 退避重试（旧版没有）。

所有对外错误都收敛为 ModelError，消息经过凭据脱敏——key 不进日志、
不进事件流。

重试预算与 Rust 侧的章节看门狗（AGENT_SIDECAR_CHAPTER_TIMEOUT_SECS，
默认 1800s）对齐。单次 chat 最坏 3 次尝试 ×（connect 10s + write 30s +
read 150s）+ 退避 ≈ 573s，而一章最多 loop.MAX_ROUNDS=5 轮 chat——只按
单次调用限流挡不住（最坏 5×573≈2865s）。ModelClient 在首次调用时用
time.monotonic 设定总预算 TOTAL_BUDGET_SECS=1500s（5 轮 × 300s），
同一 run 帧内的所有 chat/images 共享，重试与退避都不能越过；最坏情形
= 预算耗尽后已在途的一次尝试（190s）≈ 1690s < 1800s——Python 侧先以
真实原因收场，而不是被看门狗截成一句「超时」。
"""

from __future__ import annotations

import json
import random
import time
from typing import Callable

import httpx

from .errors import sanitize_error_message, summarize_http_error_body
from .types import ModelEndpoint

# 3 次尝试 + 短退避（带 jitter）：预算对齐 Rust 章节看门狗（见模块 docstring）
MAX_ATTEMPTS = 3
RETRY_DELAYS = (1.0, 2.0)
# 单 run 帧内所有模型调用的总预算（time.monotonic 秒）。
# loop.MAX_ROUNDS=5 轮 chat 共享这一预算（平均每轮 300s）。
TOTAL_BUDGET_SECS = 1500.0
# 模型响应体上限：正常补全响应在 KB 量级，8MB 已极端宽裕；
# response.json() 会把整个 body 读进内存，失控上游必须有闸门
MAX_RESPONSE_BYTES = 8 * 1024 * 1024

CHAT_PATH = "/v1/chat/completions"
IMAGES_PATH = "/v1/images/generations"


class ModelError(Exception):
    """模型调用失败（用户可读的中文消息，已脱敏）。"""


def normalize_base_url(url: str | None) -> str:
    return (url or "").strip().rstrip("/")


def _default_jitter(delay: float) -> float:
    """退避抖动：叠加均匀随机，避免多实例同时撞限流后同步重试。"""
    return delay + random.uniform(0.0, delay)


class ModelClient:
    def __init__(
        self,
        secrets: list[str] | None = None,
        transport: httpx.BaseTransport | None = None,
        sleep: Callable[[float], None] = time.sleep,
        clock: Callable[[], float] = time.monotonic,
        jitter: Callable[[float], float] = _default_jitter,
    ) -> None:
        self._client = httpx.Client(
            transport=transport,
            timeout=httpx.Timeout(150.0, connect=10.0, write=30.0),
        )
        self._secrets = [s for s in (secrets or []) if s and s.strip()]
        self._sleep = sleep
        self._clock = clock
        self._jitter = jitter
        # 总 deadline（惰性设定）：一个 ModelClient 服务一个 run 帧，
        # 首次模型调用时起算，跨所有 chat/images 调用
        self._deadline: float | None = None

    def close(self) -> None:
        self._client.close()

    # -- 公开入口 -------------------------------------------------------------

    def chat(self, endpoint: ModelEndpoint, payload: dict) -> dict:
        return self._post(endpoint, CHAT_PATH, payload, fallback="AI 资料生成失败")

    def images(self, endpoint: ModelEndpoint, payload: dict) -> dict:
        return self._post(endpoint, IMAGES_PATH, payload, fallback="地图生成失败")

    # -- 内部 -----------------------------------------------------------------

    def _remaining_budget(self) -> float:
        """距总 deadline 的剩余秒数（首次调用时惰性设定 deadline）。"""
        if self._deadline is None:
            self._deadline = self._clock() + TOTAL_BUDGET_SECS
        return self._deadline - self._clock()

    def _sleep_before_retry(self, delay: float) -> None:
        """退避带 jitter，且不越过总预算：剩余不足时缩短或跳过。"""
        delay = self._jitter(delay)
        remaining = self._remaining_budget()
        if remaining <= 0:
            return
        self._sleep(min(delay, remaining))

    def _budget_exhausted_error(self, fallback: str, api_key: str) -> ModelError:
        return ModelError(
            sanitize_error_message(
                f"{fallback}：模型调用总时长超过预算（{TOTAL_BUDGET_SECS:.0f} 秒），停止重试",
                [api_key],
            )
        )

    def _post(self, endpoint: ModelEndpoint, path: str, payload: dict, fallback: str) -> dict:
        self._ensure_ready(endpoint)
        url = endpoint.baseUrl if endpoint.useFullUrl else normalize_base_url(endpoint.baseUrl) + path
        api_key = (endpoint.apiKey or "").strip()
        headers = {"Content-Type": "application/json"}
        if api_key:
            headers["Authorization"] = f"Bearer {api_key}"

        last_error: ModelError | None = None
        for attempt in range(MAX_ATTEMPTS):
            # 总 deadline：重试与退避都不能越过（预算耗尽以真实原因收场，
            # 而不是被 Rust 看门狗截成「章节处理超时」）
            if self._remaining_budget() <= 0:
                raise self._budget_exhausted_error(fallback, api_key)
            try:
                # 流式读 + 字节上限：response.json() 会把整个 body 无界读进内存
                with self._client.stream("POST", url, json=payload, headers=headers) as response:
                    status_code = response.status_code
                    content_type = response.headers.get("content-type") or ""
                    body = _read_capped(response, MAX_RESPONSE_BYTES)
            except httpx.HTTPError as error:
                last_error = ModelError(
                    sanitize_error_message(f"{fallback}：网络请求失败：{error}", [api_key])
                )
                if attempt < MAX_ATTEMPTS - 1:
                    self._sleep_before_retry(RETRY_DELAYS[min(attempt, len(RETRY_DELAYS) - 1)])
                    continue
                raise last_error from error

            if status_code == 429 or status_code >= 500:
                last_error = self._error_from_body(status_code, content_type, body, fallback, api_key)
                if attempt < MAX_ATTEMPTS - 1:
                    self._sleep_before_retry(RETRY_DELAYS[min(attempt, len(RETRY_DELAYS) - 1)])
                    continue
                raise last_error

            if status_code >= 400:
                raise self._error_from_body(status_code, content_type, body, fallback, api_key)

            try:
                return json.loads(body)
            except ValueError as error:
                raise ModelError(sanitize_error_message(f"{fallback}：响应不是有效 JSON", [api_key])) from error

        raise last_error or ModelError(fallback)

    @staticmethod
    def _ensure_ready(endpoint: ModelEndpoint) -> None:
        if not endpoint.ready():
            raise ModelError("模型未配置（baseUrl/model 为空）")

    def _error_from_body(
        self, status_code: int, content_type: str, body: bytes, fallback: str, api_key: str
    ) -> ModelError:
        message = read_model_error_body(status_code, content_type, body, fallback)
        return ModelError(sanitize_error_message(message, [api_key]))


def _read_capped(response: httpx.Response, limit: int) -> bytes:
    chunks: list[bytes] = []
    total = 0
    for chunk in response.iter_bytes():
        total += len(chunk)
        if total > limit:
            raise ModelError(f"模型响应超出大小上限（{limit // (1024 * 1024)}MB）")
        chunks.append(chunk)
    return b"".join(chunks)


def read_model_error_body(status_code: int, content_type: str, body: bytes, fallback: str) -> str:
    """从已读到的响应体提取用户可读错误（移植自 aiBookGeneration.ts:1166-1181）。"""
    try:
        if "application/json" in content_type:
            try:
                data = json.loads(body)
            except ValueError:
                return f"{fallback} ({status_code})"
            if isinstance(data, dict):
                error = data.get("error")
                if isinstance(error, dict) and isinstance(error.get("message"), str) and error["message"]:
                    return str(error["message"])
                if isinstance(data.get("errorMsg"), str) and data["errorMsg"]:
                    return str(data["errorMsg"])
            return f"{fallback} ({status_code})"
        return summarize_http_error_body(
            body.decode("utf-8", errors="replace"), fallback, status=status_code
        )
    except Exception:  # noqa: BLE001 — 读错误体本身的异常一律回退到状态码消息
        return f"{fallback} ({status_code})"
