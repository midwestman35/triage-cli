"""Tests for triage_cli.pipeline.triage_one (orchestration only)."""
from __future__ import annotations

from datetime import datetime, timezone

import pytest

from triage_cli import pipeline
from triage_cli.models import SiteEntry, Ticket


def _ticket() -> Ticket:
    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    return Ticket(
        id=42,
        subject="audio dropouts on console",
        description="see logs",
        requester_org="Aurora 911, CO",
        tags=[],
        created_at=ts,
        updated_at=ts,
        comments=[],
    )


def _site() -> SiteEntry:
    return SiteEntry(
        friendly_name="Aurora 911, CO",
        site_name="us-co-aurora-apex",
        cnc="921d7c53-e815-4566-9692-6cbce589e1d3",
    )


def test_triage_one_no_logs_path(monkeypatch: pytest.MonkeyPatch) -> None:
    """With dd_client=None, pipeline skips Datadog and returns the LLM markdown."""
    expected = "## Summary\nstub triage note\n"

    async def fake_triage(_bundle, model=None):  # noqa: ARG001
        return expected

    # _llm_extract_anchor is not patched: dd_client=None means the pipeline
    # skips anchor extraction entirely, so the real implementation is never called.

    monkeypatch.setattr(pipeline, "_llm_triage", fake_triage)

    result = pipeline.triage_one(
        _ticket(),
        _site(),
        dd_client=None,
        window_minutes=30,
        levels=["error", "warn"],
        at=None,
        verbose=False,
        show_spinner=False,
    )
    assert result == expected
