"""Tests for triage_cli.llm.triage -- JSON-mode parsing and retry behavior."""
from __future__ import annotations

import asyncio
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


def test_triage_parses_valid_json(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(llm, "_collect_text", AsyncMock(return_value=VALID_JSON))
    out, _counts = asyncio.run(llm.triage(_bundle()))
    assert out.confidence == "medium"
    assert out.finding == "x"


def test_triage_strips_code_fence(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(llm, "_collect_text", AsyncMock(return_value=FENCED_JSON))
    out, _counts = asyncio.run(llm.triage(_bundle()))
    assert out.confidence == "medium"


def test_triage_retries_once_on_malformed(monkeypatch: pytest.MonkeyPatch) -> None:
    mock = AsyncMock(side_effect=[MALFORMED, VALID_JSON])
    monkeypatch.setattr(llm, "_collect_text", mock)
    out, _counts = asyncio.run(llm.triage(_bundle()))
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


def test_triage_redacts_phone_in_ticket_before_llm(monkeypatch) -> None:
    """Verify that ticket text is redacted before being sent to Claude."""
    captured: dict[str, str] = {}

    async def fake_collect(prompt: str, system_prompt: str, model: str) -> str:
        captured["prompt"] = prompt
        return '{"finding": "x", "confidence": "low", "evidence": [], "suggested_note": "x"}'

    import asyncio
    from datetime import UTC, datetime

    from triage_cli import llm
    from triage_cli.models import (
        AnchorSource,
        SiteEntry,
        Ticket,
        TriageBundle,
    )

    monkeypatch.setattr(llm, "_collect_text", fake_collect)

    ticket = Ticket(
        id=1,
        subject="Outage",
        description="Caller 555-123-4567 reported issue.",
        requester_org="Acme",
        created_at=datetime.now(UTC),
        updated_at=datetime.now(UTC),
        comments=[],
    )
    bundle = TriageBundle(
        ticket=ticket,
        site_entry=SiteEntry(friendly_name="Acme", site_name="acme", cnc="abc"),
        log_lines=[],
        log_truncated=False,
        anchor=datetime.now(UTC),
        anchor_source=AnchorSource.CREATED_AT,
        window_start=datetime.now(UTC),
        window_end=datetime.now(UTC),
    )

    asyncio.run(llm.triage(bundle, verbose=False))

    assert "555-123-4567" not in captured["prompt"]
    assert "<PHONE>" in captured["prompt"]


def test_triage_no_redact_kwarg_passes_raw(monkeypatch) -> None:
    captured: dict[str, str] = {}

    async def fake_collect(prompt: str, system_prompt: str, model: str) -> str:
        captured["prompt"] = prompt
        return '{"finding": "x", "confidence": "low", "evidence": [], "suggested_note": "x"}'

    import asyncio
    from datetime import UTC, datetime

    from triage_cli import llm
    from triage_cli.models import (
        AnchorSource,
        SiteEntry,
        Ticket,
        TriageBundle,
    )

    monkeypatch.setattr(llm, "_collect_text", fake_collect)

    ticket = Ticket(
        id=1,
        subject="Outage",
        description="Caller 555-123-4567 reported issue.",
        requester_org="Acme",
        created_at=datetime.now(UTC),
        updated_at=datetime.now(UTC),
        comments=[],
    )
    bundle = TriageBundle(
        ticket=ticket,
        site_entry=SiteEntry(friendly_name="Acme", site_name="acme", cnc="abc"),
        log_lines=[],
        log_truncated=False,
        anchor=datetime.now(UTC),
        anchor_source=AnchorSource.CREATED_AT,
        window_start=datetime.now(UTC),
        window_end=datetime.now(UTC),
    )

    asyncio.run(llm.triage(bundle, verbose=False, redact_enabled=False))

    assert "555-123-4567" in captured["prompt"]
    assert "<PHONE>" not in captured["prompt"]
