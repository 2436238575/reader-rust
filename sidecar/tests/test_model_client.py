"""模型客户端：URL 拼接、鉴权头、重试与错误摘要（旧传输层用例的 sidecar 重写版）。"""

import httpx
import pytest

from agent_sidecar import model_client
from agent_sidecar.model_client import (
    CHAT_PATH,
    ModelClient,
    ModelError,
    normalize_base_url,
)
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


def _client(handler, sleeps: list[float] | None = None, clock=None, jitter=None) -> ModelClient:
    # jitter 默认恒等：退避断言保持确定性；随机抖动单独用例覆盖
    return ModelClient(
        transport=httpx.MockTransport(handler),
        sleep=(lambda seconds: sleeps.append(seconds)) if sleeps is not None else (lambda s: None),
        clock=clock if clock is not None else (lambda: 0.0),
        jitter=jitter if jitter is not None else (lambda delay: delay),
    )


class SteppedClock:
    """每次被读都前进 step 秒的假时钟（deadline 逻辑不真等）。"""

    def __init__(self, step: float) -> None:
        self.now = 0.0
        self.step = step

    def __call__(self) -> float:
        value = self.now
        self.now += self.step
        return value


def test_total_budget_env_override(monkeypatch):
    # Rust 侧按 AGENT_SIDECAR_MODEL_BUDGET_SECS 下发（默认 1500）
    monkeypatch.setenv("AGENT_SIDECAR_MODEL_BUDGET_SECS", "2400")
    value = model_client._load_total_budget_secs()
    assert value == 2400.0
    assert isinstance(value, float)


def test_total_budget_env_missing_uses_default(monkeypatch):
    monkeypatch.delenv("AGENT_SIDECAR_MODEL_BUDGET_SECS", raising=False)
    assert model_client._load_total_budget_secs() == 1500.0


@pytest.mark.parametrize("bad", ["abc", "", "  ", "0", "-5", "nan", "inf"])
def test_total_budget_env_invalid_falls_back(monkeypatch, bad):
    # 非数字 / 非正数 / 非有限值一律回退默认，不让 sidecar 首轮即「预算耗尽」
    monkeypatch.setenv("AGENT_SIDECAR_MODEL_BUDGET_SECS", bad)
    assert model_client._load_total_budget_secs() == 1500.0


def test_total_budget_is_float():
    assert isinstance(model_client.TOTAL_BUDGET_SECS, float)
    assert model_client.TOTAL_BUDGET_SECS > 0


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


def test_retries_5xx_within_budget_then_raises():
    # 重试预算对齐 Rust 章节看门狗（默认 1800s）：3 次尝试 + 短退避
    attempts = {"n": 0}
    sleeps: list[float] = []

    def handler(request: httpx.Request) -> httpx.Response:
        attempts["n"] += 1
        return httpx.Response(503, json={"error": {"message": "upstream down"}})

    with pytest.raises(ModelError, match="upstream down"):
        _client(handler, sleeps).chat(_endpoint(), {})
    assert attempts["n"] == 3
    assert sleeps == [1.0, 2.0]


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
        _client(handler, []).chat(_endpoint(), {})
    assert attempts["n"] == 3


def test_oversized_response_is_rejected():
    from agent_sidecar.model_client import MAX_RESPONSE_BYTES

    def handler(request: httpx.Request) -> httpx.Response:
        # 失控上游：body 超过上限时必须在读取阶段就被掐断
        return httpx.Response(200, content=b"x" * (MAX_RESPONSE_BYTES + 1))

    with pytest.raises(ModelError, match="大小上限"):
        _client(handler).chat(_endpoint(), {})


def test_unready_endpoint_raises_without_request():
    with pytest.raises(ModelError, match="模型未配置"):
        _client(lambda request: httpx.Response(200)).chat(_endpoint(baseUrl="", model=""), {})


def test_total_deadline_stops_retries_with_real_reason():
    # 预算耗尽：不再发起尝试，以「总时长超过预算」收场（注入假时钟，不真等）
    attempts = {"n": 0}
    sleeps: list[float] = []

    def handler(request: httpx.Request) -> httpx.Response:
        attempts["n"] += 1
        return httpx.Response(503, json={"error": {"message": "upstream down"}})

    # 首次检查时预算尚余（0→800，剩 700s）→ 发起第 1 次尝试；
    # 之后时钟越过 deadline → 退避前检查即放弃
    clock = SteppedClock(step=800.0)
    with pytest.raises(ModelError, match="总时长超过预算"):
        _client(handler, sleeps, clock=clock).chat(_endpoint(), {})
    assert attempts["n"] == 1, "预算耗尽后不应再发起尝试"
    assert sleeps == [], "预算耗尽后不应退避等待"


def test_total_deadline_caps_backoff_sleep():
    # 退避不得越过 deadline：剩余时间不足时缩短/跳过退避
    attempts = {"n": 0}
    sleeps: list[float] = []

    def handler(request: httpx.Request) -> httpx.Response:
        attempts["n"] += 1
        return httpx.Response(503, json={"error": {"message": "upstream down"}})

    clock = SteppedClock(step=400.0)
    with pytest.raises(ModelError, match="总时长超过预算"):
        _client(handler, sleeps, clock=clock).chat(_endpoint(), {})
    # 第 1 次退避时预算尚余（睡眠 1.0 不变）；第 2 次退避前预算已负 → 跳过，
    # 下一轮循环的预算检查以真实原因终止
    assert attempts["n"] == 2
    assert sleeps == [1.0]


def test_deadline_spans_all_model_calls_in_one_client():
    # 一个 run 帧一个 ModelClient：deadline 跨所有 chat/images 调用，
    # 预算耗尽后第二次调用直接拒绝、不再发请求
    attempts = {"n": 0}
    holder = {"now": 0.0}

    def handler(request: httpx.Request) -> httpx.Response:
        attempts["n"] += 1
        return httpx.Response(200, json={"choices": []})

    client = ModelClient(
        transport=httpx.MockTransport(handler),
        sleep=lambda seconds: None,
        clock=lambda: holder["now"],
        jitter=lambda delay: delay,
    )
    client.chat(_endpoint(), {})  # 此刻 now=0，deadline=0+1500
    holder["now"] = 1501.0
    with pytest.raises(ModelError, match="总时长超过预算"):
        client.chat(_endpoint(), {})
    assert attempts["n"] == 1, "第二次调用在发请求前就应被拒绝"


def test_backoff_jitter_is_applied():
    # 429/5xx 退避带 jitter（默认叠加均匀随机；此用例注入固定增量验证通路）
    attempts = {"n": 0}
    sleeps: list[float] = []

    def handler(request: httpx.Request) -> httpx.Response:
        attempts["n"] += 1
        return httpx.Response(429, json={"error": {"message": "rate limited"}})

    with pytest.raises(ModelError, match="rate limited"):
        _client(handler, sleeps, jitter=lambda delay: delay + 0.5).chat(_endpoint(), {})
    assert sleeps == [1.5, 2.5]
