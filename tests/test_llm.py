"""Tests for triage_cli.llm.triage -- JSON-mode parsing and retry behavior."""
from __future__ import annotations

import asyncio
import builtins
from datetime import UTC, datetime
from unittest.mock import AsyncMock

import pytest

from triage_cli import llm
from triage_cli.models import (
    AnchorSource,
    SiteEntry,
    Ticket,
    TriageBundle,
)


def _bundle() -> TriageBundle:
    """Minimal TriageBundle for prompt input -- content doesn't matter here."""
    ts = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    return TriageBundle(
        ticket=Ticket(
            id=42,
            subject="audio dropouts",
            description="see logs",
            requester_org="Aurora 911, CO",
            tags=[],
            created_at=ts,
            updated_at=ts,
            comments=[],
        ),
        site_entry=SiteEntry(
            friendly_name="Aurora 911, CO",
            site_name="us-co-aurora-apex",
            cnc="00000000-0000-0000-0000-000000000000",
        ),
        log_lines=[],
        log_truncated=False,
        anchor=ts,
        anchor_source=AnchorSource.CREATED_AT,
        window_start=ts,
        window_end=ts,
    )


VALID_JSON = (
    '{"finding":"x","confidence":"medium","evidence":[],'
    '"suggested_note":"y","next_checks":[],"unknowns":[]}'
)
FENCED_JSON = "```json\n" + VALID_JSON + "\n```"
MALFORMED = "I'm sorry, I cannot produce JSON."


class _FakeResponse:
    def __init__(
        self,
        status_code: int,
        payload: object,
        *,
        headers: dict[str, str] | None = None,
        text: str = "",
    ) -> None:
        self.status_code = status_code
        self._payload = payload
        self.headers = headers or {}
        self.text = text or (payload if isinstance(payload, str) else "")

    def json(self) -> object:
        if isinstance(self._payload, Exception):
            raise self._payload
        return self._payload


class _FakeAsyncClient:
    response = _FakeResponse(200, {})
    requests: list[dict[str, object]] = []

    def __init__(self, *args, **kwargs) -> None:
        self.args = args
        self.kwargs = kwargs

    async def __aenter__(self):
        return self

    async def __aexit__(self, exc_type, exc, tb) -> None:
        return None

    async def post(self, url: str, *, headers: dict[str, str], json: dict[str, object]):
        self.requests.append({"url": url, "headers": headers, "json": json})
        return self.response


def _set_unleash_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LLM_PROVIDER", "unleash")
    monkeypatch.setenv("UNLEASH_API_KEY", "test-key")
    monkeypatch.setenv("UNLEASH_BASE_URL", "https://tenant.example/e-api/")
    monkeypatch.setenv("UNLEASH_ASSISTANT_ID", "assistant-123")
    monkeypatch.delenv("UNLEASH_ACCOUNT", raising=False)


def _set_openai_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LLM_PROVIDER", "openai")
    monkeypatch.setenv("OPENAI_API_KEY", "test-openai-key")
    monkeypatch.setenv("OPENAI_BASE_URL", "https://api.example/v1/")
    monkeypatch.setenv("OPENAI_MODEL", "gpt-test")


def test_triage_parses_valid_json(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(llm, "_collect_text", AsyncMock(return_value=VALID_JSON))
    out = asyncio.run(llm.triage(_bundle()))
    assert out.confidence == "medium"
    assert out.finding == "x"


def test_triage_strips_code_fence(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(llm, "_collect_text", AsyncMock(return_value=FENCED_JSON))
    out = asyncio.run(llm.triage(_bundle()))
    assert out.confidence == "medium"


def test_triage_retries_once_on_malformed(monkeypatch: pytest.MonkeyPatch) -> None:
    mock = AsyncMock(side_effect=[MALFORMED, VALID_JSON])
    monkeypatch.setattr(llm, "_collect_text", mock)
    out = asyncio.run(llm.triage(_bundle()))
    assert out.finding == "x"
    assert mock.await_count == 2


def test_triage_raises_after_retry_failure(monkeypatch: pytest.MonkeyPatch) -> None:
    mock = AsyncMock(side_effect=[MALFORMED, MALFORMED])
    monkeypatch.setattr(llm, "_collect_text", mock)
    with pytest.raises(RuntimeError, match="invalid TriageReport JSON after retry"):
        asyncio.run(llm.triage(_bundle()))


def test_triage_verbose_logs_retry(
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
) -> None:
    mock = AsyncMock(side_effect=[MALFORMED, VALID_JSON])
    monkeypatch.setattr(llm, "_collect_text", mock)
    with caplog.at_level("WARNING", logger="triage_cli.llm"):
        asyncio.run(llm.triage(_bundle(), verbose=True))
    assert any("retrying" in r.message for r in caplog.records)


def test_provider_default_is_unleash(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("LLM_PROVIDER", raising=False)
    assert llm._resolve_provider_name() == "unleash"


def test_codex_alias_uses_openai_provider(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LLM_PROVIDER", "codex")
    assert llm._resolve_provider_name() == "openai"


def test_claude_agent_sdk_is_imported_lazily(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LLM_PROVIDER", "unleash")

    real_import = builtins.__import__

    def blocked_import(name, *args, **kwargs):
        if name == "claude_agent_sdk":
            raise AssertionError("claude provider should not import for unleash selection")
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", blocked_import)

    assert isinstance(llm._provider_for_name("unleash"), llm.UnleashProvider)


def test_unleash_request_shape_and_text_parsing(monkeypatch: pytest.MonkeyPatch) -> None:
    _set_unleash_env(monkeypatch)
    monkeypatch.setenv("UNLEASH_ACCOUNT", "analyst@example.com")
    _FakeAsyncClient.requests = []
    _FakeAsyncClient.response = _FakeResponse(
        200,
        {
            "type": "Full",
            "requestId": "req-ok",
            "message": {
                "role": "Assistant",
                "parts": [
                    {"type": "Text", "text": "hello"},
                    {"type": "InlineReference", "text": "ignored"},
                    {"type": "Text", "text": " world"},
                ],
            },
        },
    )
    monkeypatch.setattr(llm.httpx, "AsyncClient", _FakeAsyncClient)

    text = asyncio.run(llm._collect_text("user prompt", "system prompt", "ignored"))

    assert text == "hello world"
    assert len(_FakeAsyncClient.requests) == 1
    request = _FakeAsyncClient.requests[0]
    assert request["url"] == "https://tenant.example/e-api/chats"
    assert request["headers"] == {
        "Authorization": "Bearer test-key",
        "Accept": "application/json",
        "Content-Type": "application/json",
        "unleash-account": "analyst@example.com",
    }
    assert request["json"] == {
        "assistantId": "assistant-123",
        "stream": False,
        "messages": [
            {"role": "System", "text": "system prompt"},
            {"role": "User", "text": "user prompt"},
        ],
    }


def test_unleash_missing_config_fails_before_network(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("LLM_PROVIDER", "unleash")
    monkeypatch.delenv("UNLEASH_API_KEY", raising=False)
    monkeypatch.setenv("UNLEASH_ASSISTANT_ID", "assistant-123")

    class ForbiddenClient:
        def __init__(self, *args, **kwargs) -> None:
            raise AssertionError("network client should not be constructed")

    monkeypatch.setattr(llm.httpx, "AsyncClient", ForbiddenClient)

    with pytest.raises(RuntimeError, match="UNLEASH_API_KEY"):
        asyncio.run(llm._collect_text("user", "system", "ignored"))


def test_openai_request_shape_and_text_parsing(monkeypatch: pytest.MonkeyPatch) -> None:
    _set_openai_env(monkeypatch)
    _FakeAsyncClient.requests = []
    _FakeAsyncClient.response = _FakeResponse(
        200,
        {
            "id": "resp-ok",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "hello"},
                        {"type": "refusal", "text": "ignored"},
                        {"type": "output_text", "text": " world"},
                    ],
                }
            ],
        },
    )
    monkeypatch.setattr(llm.httpx, "AsyncClient", _FakeAsyncClient)

    text = asyncio.run(llm._collect_text("user prompt", "system prompt", "gpt-test"))

    assert text == "hello world"
    assert len(_FakeAsyncClient.requests) == 1
    request = _FakeAsyncClient.requests[0]
    assert request["url"] == "https://api.example/v1/responses"
    assert request["headers"] == {
        "Authorization": "Bearer test-openai-key",
        "Accept": "application/json",
        "Content-Type": "application/json",
    }
    assert request["json"] == {
        "model": "gpt-test",
        "instructions": "system prompt",
        "input": "user prompt",
        "store": False,
    }


def test_openai_missing_config_fails_before_network(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("LLM_PROVIDER", "openai")
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)

    class ForbiddenClient:
        def __init__(self, *args, **kwargs) -> None:
            raise AssertionError("network client should not be constructed")

    monkeypatch.setattr(llm.httpx, "AsyncClient", ForbiddenClient)

    with pytest.raises(RuntimeError, match="OPENAI_API_KEY"):
        asyncio.run(llm._collect_text("user", "system", "gpt-test"))


def test_openai_http_error_includes_request_id(monkeypatch: pytest.MonkeyPatch) -> None:
    _set_openai_env(monkeypatch)
    _FakeAsyncClient.requests = []
    _FakeAsyncClient.response = _FakeResponse(
        401,
        {"error": {"message": "bad key"}},
        headers={"x-request-id": "req-openai"},
    )
    monkeypatch.setattr(llm.httpx, "AsyncClient", _FakeAsyncClient)

    with pytest.raises(RuntimeError, match="HTTP 401: bad key.*req-openai"):
        asyncio.run(llm._collect_text("user", "system", "gpt-test"))
