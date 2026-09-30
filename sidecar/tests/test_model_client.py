"""模型客户端：URL 拼接、鉴权头、重试与错误摘要（旧传输层用例的 sidecar 重写版）。"""

import httpx
import pytest

from agent_sidecar.model_client import CHAT_PATH, ModelClient, ModelError, normalize_base_url
from agent_sidecar.types import ImageModelEndpoint, ModelEndpoint

CLOUDFLARE_HTML = (
    "<!DOCTYPE html>\n"
    "<html><head><title>grandy.fun | 524: A timeout occurred</title></head>\n"
    '<body><span class="code-label">Error code 524</span><p>boom</p></body></html>'
)


def _endpoint(**overrides) -> ModelEndpoint:
    values = {"baseUrl": "http://localhost:8825", "apiKey": "", "model": "m1", "useFullUrl": False}
    values.update(overrides)
    return ModelEndpoint(**values)


def _client(handler, sleeps: list[float] | None = None) -> ModelClient:
    return ModelClient(
        transport=httpx.MockTransport(handler),
        sleep=(lambda seconds: sleeps.append(seconds)) if sleeps is not None else (lambda s: None),
    )


def test_normalize_base_url_strips_trailing_slashes():
    assert normalize_base_url(" http://a.example.test/ ") == "http://a.example.test"
    assert normalize_base_url("http://a.example.test///") == "http://a.example.test"


def test_joins_base_url_with_chat_path():
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        return httpx.Response(200, json={"choices": []})

    _client(handler).chat(_endpoint(baseUrl="http://localhost:8825/"), {"x": 1})
    assert seen["url"] == "http://localhost:8825" + CHAT_PATH


def test_full_url_endpoint_is_used_verbatim():
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        return httpx.Response(200, json={"choices": []})

    _client(handler).chat(
        _endpoint(baseUrl="https://gateway.example.test/custom/chat", useFullUrl=True), {}
    )
    assert seen["url"] == "https://gateway.example.test/custom/chat"


def test_bearer_header_only_when_key_present():
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen.setdefault("auth", []).append(request.headers.get("authorization"))
        return httpx.Response(200, json={"choices": []})

    _client(handler).chat(_endpoint(apiKey="text-key"), {})
    _client(handler).chat(_endpoint(), {})
    assert seen["auth"] == ["Bearer text-key", None]


def test_retries_429_then_succeeds():
    attempts = {"n": 0}
    sleeps: list[float] = []

    def handler(request: httpx.Request) -> httpx.Response:
        attempts["n"] += 1
        if attempts["n"] == 1:
            return httpx.Response(429, json={"error": {"message": "rate limited"}})
        return httpx.Response(200, json={"choices": []})

    _client(handler, sleeps).chat(_endpoint(), {})
    assert attempts["n"] == 2
    assert sleeps == [1.0]


def test_retries_5xx_up_to_three_attempts_then_raises():
    attempts = {"n": 0}

    def handler(request: httpx.Request) -> httpx.Response:
        attempts["n"] += 1
        return httpx.Response(503, json={"error": {"message": "upstream down"}})

    with pytest.raises(ModelError, match="upstream down"):
        _client(handler).chat(_endpoint(), {})
    assert attempts["n"] == 3


def test_4xx_is_not_retried():
    attempts = {"n": 0}

    def handler(request: httpx.Request) -> httpx.Response:
        attempts["n"] += 1
        return httpx.Response(401, json={"error": {"message": "bad key"}})

    with pytest.raises(ModelError, match="bad key"):
        _client(handler).chat(_endpoint(), {})
    assert attempts["n"] == 1


def test_html_error_page_is_summarized():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(500, text=CLOUDFLARE_HTML, headers={"content-type": "text/html"})

    with pytest.raises(ModelError) as exc_info:
        _client(handler).chat(_endpoint(), {})
    assert (
        str(exc_info.value)
        == "AI 资料生成失败 (500)，服务返回 HTML 错误页，错误码 524：grandy.fun | 524: A timeout occurred"
    )


def test_error_message_is_redacted_for_known_key():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(400, json={"error": {"message": "invalid key sk-secret999 provided"}})

    with pytest.raises(ModelError) as exc_info:
        _client(handler).chat(_endpoint(apiKey="sk-secret999"), {})
    assert "sk-secret999" not in str(exc_info.value)
    assert "***" in str(exc_info.value)


def test_images_uses_images_path_and_size():
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        seen["body"] = request.read()
        return httpx.Response(200, json={"data": [{"b64_json": "aGk="}]})

    endpoint = ImageModelEndpoint(baseUrl="http://localhost:8826", apiKey="k", model="img", imageSize="1024x1024")
    data = _client(handler).images(endpoint, {"prompt": "p", "size": "1024x1024"})
    assert seen["url"] == "http://localhost:8826/v1/images/generations"
    assert b'"prompt"' in seen["body"]
    assert data["data"][0]["b64_json"] == "aGk="


def test_network_error_is_wrapped_and_retried():
    attempts = {"n": 0}

    def handler(request: httpx.Request) -> httpx.Response:
        attempts["n"] += 1
        raise httpx.ConnectError("connection refused", request=request)

    with pytest.raises(ModelError, match="网络请求失败"):
        _client(handler).chat(_endpoint(), {})
    assert attempts["n"] == 3


def test_unready_endpoint_raises_without_request():
    with pytest.raises(ModelError, match="模型未配置"):
        _client(lambda request: httpx.Response(200)).chat(_endpoint(baseUrl="", model=""), {})
