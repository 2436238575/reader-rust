r"""HTTP 错误响应体的可读摘要。

移植自 httpError.ts，并新增凭据脱敏（scrub_credentials / redact_secrets）：
sidecar 经 stdin 持有模型 API key 并直连模型，错误摘要进入事件流与日志前
必须把 key 抹掉——前端旧版不需要这一步，因为 key 本来就在浏览器里。

移植说明：JS 的 `\b` 词边界按 ASCII \w 计算，Python 的 \b 把 CJK 算作
词字符，因此 `[45]\d{2}` 的边界用显式断言复刻 JS 语义。
"""

from __future__ import annotations

import re
from collections.abc import Iterable

DOCTYPE_RE = re.compile(r"^\s*<(?:!doctype\s+html|html|head|body|div|span|p|h1)(?![A-Za-z0-9_])", re.IGNORECASE)
HTML_TAG_RE = re.compile(r"<html[\s>]", re.IGNORECASE)
HTML_CLOSE_RE = re.compile(r"</(?:html|body|head)>", re.IGNORECASE)
TITLE_RE = re.compile(r"<title[^>]*>([\s\S]*?)</title>", re.IGNORECASE)
ERROR_CODE_RE = re.compile(r"(?:Error code|errorcode[_-])\s*(\d{3})", re.IGNORECASE)
# JS \b 是 ASCII 词边界；这里用显式断言等价复刻，避免 CJK 上下文失配
BARE_CODE_RE = re.compile(r"(?<![0-9A-Za-z_])([45]\d{2})(?![0-9A-Za-z_])")
TAG_RE = re.compile(r"<[^>]*>")

_BEARER_RE = re.compile(r"(?i)bearer\s+[A-Za-z0-9._\-]+")
_CREDENTIAL_FIELD_RE = re.compile(
    r"(?i)(authorization|api[-_]?key|api[-_]?token)(\s*[\"']?\s*[:=]\s*[\"']?)(?!bearer\b)([^\s\"',&}]+)"
)


def summarize_http_error_body(
    raw: str,
    fallback: str,
    status: int | None = None,
    max_length: int = 260,
) -> str:
    status_text = f" ({status})" if status else ""
    trimmed = raw.strip()
    if not trimmed:
        return f"{fallback}{status_text}"

    if _looks_like_html(trimmed):
        title = _extract_html_title(trimmed)
        code = _extract_html_error_code(trimmed) or (str(status) if status else "")
        detail = f"服务返回 HTML 错误页{('，错误码 ' + code) if code else ''}{('：' + title) if title else ''}"
        return "，".join([f"{fallback}{status_text}", detail])

    text = collapse_whitespace(trimmed)
    if len(text) > max_length:
        return f"{fallback}{status_text}：{text[:max_length]}..."
    return text or f"{fallback}{status_text}"


def summarize_display_error(raw: str, max_length: int = 180) -> str:
    trimmed = raw.strip()
    if not trimmed:
        return ""

    if _looks_like_html(trimmed):
        title = _extract_html_title(trimmed)
        code = _extract_html_error_code(trimmed)
        return f"服务返回 HTML 错误页{('，错误码 ' + code) if code else ''}{('：' + title) if title else ''}"

    text = collapse_whitespace(trimmed)
    return f"{text[:max_length]}..." if len(text) > max_length else text


def collapse_whitespace(value: str) -> str:
    return re.sub(r"\s+", " ", _decode_html_entities(value)).strip()


def _looks_like_html(value: str) -> bool:
    return bool(
        DOCTYPE_RE.match(value) or HTML_TAG_RE.search(value) or HTML_CLOSE_RE.search(value)
    )


def _extract_html_title(value: str) -> str:
    match = TITLE_RE.search(value)
    return collapse_whitespace(_strip_tags(match.group(1))) if match else ""


def _extract_html_error_code(value: str) -> str:
    match = ERROR_CODE_RE.search(value) or BARE_CODE_RE.search(value)
    return match.group(1) if match else ""


def _strip_tags(value: str) -> str:
    return TAG_RE.sub(" ", value)


def _decode_html_entities(value: str) -> str:
    value = re.sub(r"&nbsp;", " ", value, flags=re.IGNORECASE)
    value = value.replace("&amp;", "&")
    value = value.replace("&lt;", "<")
    value = value.replace("&gt;", ">")
    value = value.replace("&quot;", '"')
    value = value.replace("&#39;", "'")

    def _numeric(match: re.Match[str]) -> str:
        code = int(match.group(1))
        # 复刻 JS String.fromCharCode 的模 65536 行为
        return chr(code % 65536)

    return re.sub(r"&#(\d+);", _numeric, value)


def redact_secrets(text: str, secrets: Iterable[str]) -> str:
    """把已知密钥串替换为 ***（短于 4 字符的串不处理，避免误伤）。"""
    result = text
    for secret in secrets:
        candidate = (secret or "").strip()
        if len(candidate) >= 4 and candidate in result:
            result = result.replace(candidate, "***")
    return result


def scrub_credentials(text: str) -> str:
    """抹掉 Bearer 令牌与 authorization/api-key 字段值。"""
    text = _BEARER_RE.sub("Bearer ***", text)
    return _CREDENTIAL_FIELD_RE.sub(lambda m: f"{m.group(1)}{m.group(2)}***", text)


def sanitize_error_message(text: str, secrets: Iterable[str] = ()) -> str:
    """错误消息出 sidecar 前的最后一步：先抹已知 key，再抹凭据字段形态。"""
    return scrub_credentials(redact_secrets(text, secrets))
