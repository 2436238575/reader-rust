"""httpError.test.ts 3 例的 1:1 移植 + 新增脱敏用例。"""

from agent_sidecar.errors import (
    redact_secrets,
    sanitize_error_message,
    scrub_credentials,
    summarize_display_error,
    summarize_http_error_body,
)

CLOUDFLARE_TIMEOUT_HTML = (
    "<!DOCTYPE html>\n"
    "<html><head><title>grandy.fun | 524: A timeout occurred</title></head>\n"
    '<body><span class="code-label">Error code 524</span><p>' + "x" * 1000 + "</p></body></html>"
)


def test_summarizes_html_error_pages_for_model_requests():
    assert (
        summarize_http_error_body(CLOUDFLARE_TIMEOUT_HTML, "AI 资料生成失败", status=524)
        == "AI 资料生成失败 (524)，服务返回 HTML 错误页，错误码 524：grandy.fun | 524: A timeout occurred"
    )


def test_summarizes_saved_display_errors_without_leaking_full_html():
    summary = summarize_display_error(CLOUDFLARE_TIMEOUT_HTML)
    assert summary == "服务返回 HTML 错误页，错误码 524：grandy.fun | 524: A timeout occurred"
    assert "<!DOCTYPE" not in summary
    assert len(summary) < 100


def test_truncates_long_plain_text_errors():
    summary = summarize_display_error("失败：" + "网络超时" * 100, 40)
    assert summary.endswith("...")
    assert len(summary) <= 43


def test_empty_body_falls_back_to_status_text():
    assert summarize_http_error_body("", "AI 资料生成失败", status=502) == "AI 资料生成失败 (502)"
    assert summarize_http_error_body("   ", "AI 资料生成失败") == "AI 资料生成失败"


def test_redact_secrets_masks_known_keys():
    message = "调用 sk-abcd1234 失败：unauthorized"
    assert "sk-abcd1234" not in redact_secrets(message, ["sk-abcd1234"])
    assert "***" in redact_secrets(message, ["sk-abcd1234"])
    # 短串不处理，避免误伤
    assert redact_secrets("abc 失败", ["abc"]) == "abc 失败"


def test_scrub_credentials_masks_bearer_and_key_fields():
    text = 'Authorization: Bearer eyJhbGci.payload.sig and {"apiKey": "sk-xyz9876"} done'
    scrubbed = scrub_credentials(text)
    assert "eyJhbGci" not in scrubbed
    assert "sk-xyz9876" not in scrubbed
    assert "Bearer ***" in scrubbed
    assert '"apiKey": "***"' in scrubbed


def test_sanitize_error_message_combines_both():
    message = "Bearer sk-live9999 响应错误"
    result = sanitize_error_message(message, ["sk-live9999"])
    assert "sk-live9999" not in result
