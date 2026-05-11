"""Tests for the LLM provider abstraction layer."""
from __future__ import annotations

import pytest


def test_get_provider_default_is_unleash(monkeypatch):
    monkeypatch.delenv("LLM_PROVIDER", raising=False)
    import importlib

    from triage_cli import llm
    importlib.reload(llm)
    provider = llm.get_provider()
    assert provider.name == "unleash"


def test_get_provider_claude(monkeypatch):
    monkeypatch.setenv("LLM_PROVIDER", "claude")
    import importlib

    from triage_cli import llm
    importlib.reload(llm)
    provider = llm.get_provider()
    assert provider.name == "claude"


def test_get_provider_openai(monkeypatch):
    monkeypatch.setenv("LLM_PROVIDER", "openai")
    import importlib

    from triage_cli import llm
    importlib.reload(llm)
    provider = llm.get_provider()
    assert provider.name == "openai"


def test_get_provider_unknown_raises(monkeypatch):
    monkeypatch.setenv("LLM_PROVIDER", "grok")
    import importlib

    from triage_cli import llm
    importlib.reload(llm)
    with pytest.raises(ValueError, match="Unknown LLM_PROVIDER"):
        llm.get_provider()


def test_unleash_complete_calls_api(monkeypatch):
    """UnleashProvider.complete() sends the right shape to the API."""
    from triage_cli.providers.unleash import UnleashProvider

    monkeypatch.setenv("UNLEASH_API_KEY", "test-key")
    monkeypatch.setenv("UNLEASH_ASSISTANT_ID", "asst-1")

    captured = {}

    async def fake_post(self, url, *, headers, json, timeout=None, **kwargs):
        captured["url"] = url
        captured["payload"] = json

        class MockResp:
            status_code = 200
            headers = {}

            def raise_for_status(self):
                pass

            def json(self):
                return [{"message": {"parts": [{"type": "Text", "text": "hello"}]}}]

        return MockResp()

    import httpx
    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)

    import asyncio
    provider = UnleashProvider()
    result = asyncio.run(provider.complete(
        prompt="test prompt",
        system_prompt="system",
        model="claude-sonnet-4-6",
    ))
    assert result == "hello"
    assert "chats" in captured["url"]


def test_openai_complete_calls_responses_api(monkeypatch):
    """OpenAIResponsesProvider.complete() posts to /responses."""
    from triage_cli.providers.openai import OpenAIResponsesProvider

    monkeypatch.setenv("OPENAI_API_KEY", "sk-test")

    captured = {}

    async def fake_post(self, url, *, headers, json, timeout=None, **kwargs):
        captured["url"] = url

        class MockResp:
            status_code = 200
            headers = {}

            def raise_for_status(self):
                pass

            def json(self):
                return {
                    "output": [
                        {
                            "type": "message",
                            "content": [{"type": "output_text", "text": "response text"}],
                        }
                    ]
                }

        return MockResp()

    import httpx
    monkeypatch.setattr(httpx.AsyncClient, "post", fake_post)

    import asyncio
    provider = OpenAIResponsesProvider()
    result = asyncio.run(provider.complete(
        prompt="test",
        system_prompt="sys",
        model="gpt-5.5",
    ))
    assert result == "response text"
    assert "/responses" in captured["url"]
