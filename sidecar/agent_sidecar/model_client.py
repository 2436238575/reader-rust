"""OpenAI 兼容模型客户端（同步 httpx）。

取代前端旧版的 fetch 三路径（直连 / 浏览器代理 / server 模式）：sidecar 从
Rust 拿到的就是解析后的 endpoint（baseUrl/apiKey/model/useFullUrl），
只剩「拼接 URL 并直连」一条路。新增 429/5xx 退避重试（旧版没有）。

所有对外错误都收敛为 ModelError，消息经过凭据脱敏——key 不进日志、
不进事件流。
"""

from __future__ import annotations

import time
from typing import Callable

import httpx

from .errors import sanitize_error_message, summarize_http_error_body
from .types import ModelEndpoint

MAX_ATTEMPTS = 3
RETRY_DELAYS = (1.0, 3.0)

CHAT_PATH = "/v1/chat/completions"
IMAGES_PATH = "/v1/images/generations"


class ModelError(Exception):
    """模型调用失败（用户可读的中文消息，已脱敏）。"""


def normalize_base_url(url: str | None) -> str:
    return (url or "").strip().rstrip("/")


class ModelClient:
    def __init__(
        self,
        secrets: list[str] | None = None,
        transport: httpx.BaseTransport | None = None,
        sleep: Callable[[float], None] = time.sleep,
    ) -> None:
        self._client = httpx.Client(
            transport=transport,
            timeout=httpx.Timeout(180.0, connect=10.0, write=30.0),
        )
        self._secrets = [s for s in (secrets or []) if s and s.strip()]
        self._sleep = sleep

    def close(self) -> None:
        self._client.close()

    # -- 公开入口 -------------------------------------------------------------

    def chat(self, endpoint: ModelEndpoint, payload: dict) -> dict:
        return self._post(endpoint, CHAT_PATH, payload, fallback="AI 资料生成失败")

    def images(self, endpoint: ModelEndpoint, payload: dict) -> dict:
        return self._post(endpoint, IMAGES_PATH, payload, fallback="地图生成失败")

    # -- 内部 -----------------------------------------------------------------

    def _post(self, endpoint: ModelEndpoint, path: str, payload: dict, fallback: str) -> dict:
        self._ensure_ready(endpoint)
        url = endpoint.baseUrl if endpoint.useFullUrl else normalize_base_url(endpoint.baseUrl) + path
        api_key = (endpoint.apiKey or "").strip()
        headers = {"Content-Type": "application/json"}
        if api_key:
            headers["Authorization"] = f"Bearer {api_key}"

        last_error: ModelError | None = None
        for attempt in range(MAX_ATTEMPTS):
            try:
                response = self._client.post(url, json=payload, headers=headers)
            except httpx.HTTPError as error:
                last_error = ModelError(
                    sanitize_error_message(f"{fallback}：网络请求失败：{error}", [api_key])
                )
                if attempt < MAX_ATTEMPTS - 1:
                    self._sleep(RETRY_DELAYS[min(attempt, len(RETRY_DELAYS) - 1)])
                    continue
                raise last_error from error

            if response.status_code == 429 or response.status_code >= 500:
                last_error = self._error_from_response(response, fallback, api_key)
                if attempt < MAX_ATTEMPTS - 1:
                    self._sleep(RETRY_DELAYS[min(attempt, len(RETRY_DELAYS) - 1)])
                    continue
                raise last_error

            if response.status_code >= 400:
                raise self._error_from_response(response, fallback, api_key)

            try:
                return response.json()
            except ValueError as error:
                raise ModelError(sanitize_error_message(f"{fallback}：响应不是有效 JSON", [api_key])) from error

        raise last_error or ModelError(fallback)

    @staticmethod
    def _ensure_ready(endpoint: ModelEndpoint) -> None:
        if not endpoint.ready():
            raise ModelError("模型未配置（baseUrl/model 为空）")

    def _error_from_response(self, response: httpx.Response, fallback: str, api_key: str) -> ModelError:
        message = read_model_error(response, fallback)
        return ModelError(sanitize_error_message(message, [api_key]))


def read_model_error(response: httpx.Response, fallback: str) -> str:
    """移植自 aiBookGeneration.ts:1166-1181。"""
    try:
        content_type = response.headers.get("content-type") or ""
        if "application/json" in content_type:
            try:
                data = response.json()
            except ValueError:
                return f"{fallback} ({response.status_code})"
            if isinstance(data, dict):
                error = data.get("error")
                if isinstance(error, dict) and isinstance(error.get("message"), str) and error["message"]:
                    return str(error["message"])
                if isinstance(data.get("errorMsg"), str) and data["errorMsg"]:
                    return str(data["errorMsg"])
            return f"{fallback} ({response.status_code})"
        return summarize_http_error_body(response.text, fallback, status=response.status_code)
    except Exception:  # noqa: BLE001 — 读错误体本身的异常一律回退到状态码消息
        return f"{fallback} ({response.status_code})"
