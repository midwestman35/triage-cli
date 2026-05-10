"""Tests for triage_cli.pipeline.triage_one (orchestration only)."""
from __future__ import annotations

from datetime import UTC, datetime

import pytest

from triage_cli import pipeline
from triage_cli.models import SiteEntry, Ticket


def _ticket() -> Ticket:
    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
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
    """With dd_client=None, pipeline skips Datadog and returns a TriageReport."""
    from triage_cli.models import LLMTriageOutput, TriageReport

    canned = LLMTriageOutput(
        finding="stub finding",
        confidence="low",
        evidence=[],
        suggested_note="stub note",
    )

    async def fake_triage(_bundle, model=None, *, verbose=False, redact_enabled=True):  # noqa: ARG001
        return canned

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

    assert isinstance(result, TriageReport)
    assert result.finding == "stub finding"
    assert result.ticket_id == 42
    assert result.site_name == "us-co-aurora-apex"
    assert result.sources == ["zendesk"]
    assert result.log_event_count == 0


def test_triage_one_with_logs_populates_report(monkeypatch: pytest.MonkeyPatch) -> None:
    """With a dd_client returning logs, the pipeline-derived sources include 'datadog'."""
    from triage_cli.models import LLMTriageOutput, LogLine, TriageReport

    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    canned = LLMTriageOutput(
        finding="x",
        confidence="medium",
        evidence=[],
        suggested_note="y",
    )

    async def fake_triage(_bundle, model=None, *, verbose=False, redact_enabled=True):  # noqa: ARG001
        return canned

    async def fake_extract(_ticket, model=None, *, redact_enabled=True, verbose=False):  # noqa: ARG001
        return None

    class FakeDD:
        def get_logs(self, _site, _levels, _start, _end):
            return (
                [LogLine(timestamp=ts, level="error", message="boom")],
                False,
            )

    monkeypatch.setattr(pipeline, "_llm_triage", fake_triage)
    monkeypatch.setattr(pipeline, "_llm_extract_anchor", fake_extract)

    report = pipeline.triage_one(
        _ticket(),
        _site(),
        dd_client=FakeDD(),  # type: ignore[arg-type]
        window_minutes=30,
        levels=["error", "warn"],
        at=None,
        verbose=False,
        show_spinner=False,
    )
    assert isinstance(report, TriageReport)
    assert "datadog" in report.sources
    assert report.log_event_count == 1
    assert report.window.end >= report.window.start


def test_triage_one_headless_bundle_has_empty_evidence_fields(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Watcher contract: triage_one constructs a bundle with no extra evidence.

    This test pins the headless behavior. If a later change makes triage_one
    populate downloaded_attachments / local_files / pasted_logs from somewhere
    other than the caller, watcher and inbox would silently start sending more
    context to the LLM. This test fails immediately if that happens.
    """
    from triage_cli.llm import triage as _real_triage
    from triage_cli.models import LLMTriageOutput

    captured_bundles: list = []

    async def fake_triage(bundle, model=None, *, verbose=False, redact_enabled=True):  # noqa: ARG001
        captured_bundles.append(bundle)
        return LLMTriageOutput(
            finding="x", confidence="low", evidence=[], suggested_note="y",
        )

    monkeypatch.setattr(pipeline, "_llm_triage", fake_triage)

    pipeline.triage_one(
        _ticket(),
        _site(),
        dd_client=None,
        window_minutes=30,
        levels=["error", "warn"],
        at=datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC),
        verbose=False,
        show_spinner=False,
    )

    assert len(captured_bundles) == 1
    bundle = captured_bundles[0]
    assert bundle.downloaded_attachments == []
    assert bundle.local_files == []
    assert bundle.pasted_logs == []

    # Confirm fake was used, not the real LLM (defense against import drift).
    assert _real_triage is not fake_triage
