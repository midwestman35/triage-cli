"""Tests for triage_cli.investigation: session orchestration + assessment wiring."""
from __future__ import annotations

from datetime import UTC, datetime

import pytest

from triage_cli import evidence, investigation
from triage_cli.investigation import (
    InvestigationSession,
    add_source,
    build_report,
    run_assessment,
    to_assessment_prompt,
)
from triage_cli.models import Comment, LLMTriageOutput, Ticket, TriageReport
from triage_cli.timeline import TimelineEvent


def _ticket() -> Ticket:
    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    return Ticket(
        id=42,
        subject="audio dropouts",
        description="customer reports audio loss",
        requester_org="Aurora 911, CO",
        tags=["p1"],
        created_at=ts,
        updated_at=ts,
        comments=[
            Comment(
                author="alice",
                body="started after reboot",
                created_at=datetime(2026, 5, 7, 12, 5, tzinfo=UTC),
                is_public=True,
            ),
        ],
    )


def test_add_source_appends_and_merges() -> None:
    session = InvestigationSession(ticket=_ticket())
    src1, evs1 = evidence.from_ticket(session.ticket)
    add_source(session, src1, evs1)
    src2, evs2 = evidence.from_comments(session.ticket)
    add_source(session, src2, evs2)
    assert len(session.sources) == 2
    assert len(session.timeline) == 2
    # Ticket created at 12:00, comment at 12:05 — chronological.
    assert session.timeline[0].kind == "ticket_created"
    assert session.timeline[1].kind == "zendesk_comment"


def test_to_assessment_prompt_contains_manifest_and_timeline() -> None:
    session = InvestigationSession(ticket=_ticket())
    src1, evs1 = evidence.from_ticket(session.ticket)
    add_source(session, src1, evs1)
    src2, evs2 = evidence.from_pasted_text(
        "2026-05-07T12:03:00Z ERROR sip register failed", label="paste-1"
    )
    add_source(session, src2, evs2)

    prompt = to_assessment_prompt(session)
    assert "# Ticket #42" in prompt
    assert "audio dropouts" in prompt
    assert "## Description" in prompt
    assert "# Evidence sources" in prompt
    assert "pasted_text: paste-1" in prompt
    assert "# Timeline" in prompt
    assert "sip register failed" in prompt
    # Comment bodies should NOT appear in the prompt body when only the ticket
    # is sourced (we didn't add from_comments).
    assert "started after reboot" not in prompt


def test_to_assessment_prompt_truncates_long_timelines() -> None:
    session = InvestigationSession(ticket=_ticket())
    src, _ = evidence.from_ticket(session.ticket)
    base = datetime(2026, 5, 7, 13, 0, 0, tzinfo=UTC)
    events = [
        TimelineEvent(
            timestamp=base.replace(second=i % 60, minute=(i // 60)),
            source="bulk", kind="log", message=f"event-{i}",
        )
        for i in range(10)
    ]
    add_source(session, src, events)
    prompt = to_assessment_prompt(session, timeline_cap=3)
    assert "additional event(s) truncated" in prompt
    assert "event-0" in prompt
    assert "event-9" not in prompt


def test_build_report_derives_window_and_sources() -> None:
    session = InvestigationSession(ticket=_ticket())
    src1, evs1 = evidence.from_ticket(session.ticket)
    add_source(session, src1, evs1)
    src2, evs2 = evidence.from_pasted_text(
        "2026-05-07T13:00:00Z INFO hi\n2026-05-07T13:30:00Z INFO bye",
        label="p",
    )
    add_source(session, src2, evs2)
    llm_out = LLMTriageOutput(
        finding="x", confidence="medium", evidence=[], suggested_note="note",
        summary="this is the summary", correlation=["a", "b"],
    )
    report = build_report(session, llm_out)
    assert isinstance(report, TriageReport)
    assert report.ticket_id == 42
    assert report.site_name is None
    assert report.window is not None
    assert report.window.start == datetime(2026, 5, 7, 12, 0, tzinfo=UTC)
    assert report.window.end == datetime(2026, 5, 7, 13, 30, tzinfo=UTC)
    # Three timestamped events: ticket_created + two parsed lines.
    assert report.log_event_count == 3
    assert any("zendesk_ticket" in s for s in report.sources)
    assert any("pasted_text" in s for s in report.sources)
    assert report.summary == "this is the summary"
    assert report.correlation == ["a", "b"]


def test_run_assessment_uses_llm_assess(monkeypatch: pytest.MonkeyPatch) -> None:
    session = InvestigationSession(ticket=_ticket())
    src1, evs1 = evidence.from_ticket(session.ticket)
    add_source(session, src1, evs1)

    canned = LLMTriageOutput(
        finding="probable cause: reboot wiped sip cache",
        confidence="medium",
        evidence=[],
        suggested_note="please verify sip cache",
        summary="audio dropouts after reboot",
        correlation=["reboot at 12:00 lines up with first user report"],
    )

    async def fake_assess(_session, model=None, *, verbose=False):  # noqa: ARG001
        return canned

    # Patch the symbol on the llm module since investigation imports it lazily.
    from triage_cli import llm as llm_mod
    monkeypatch.setattr(llm_mod, "assess", fake_assess)

    report = run_assessment(session, verbose=False)
    assert report.finding.startswith("probable cause")
    assert report.summary == "audio dropouts after reboot"
    assert session.report is report  # stored on the session


def test_run_assessment_propagates_runtime_error(monkeypatch: pytest.MonkeyPatch) -> None:
    session = InvestigationSession(ticket=_ticket())
    src1, evs1 = evidence.from_ticket(session.ticket)
    add_source(session, src1, evs1)

    async def boom(_session, model=None, *, verbose=False):  # noqa: ARG001
        raise RuntimeError("transport blew up")

    from triage_cli import llm as llm_mod
    monkeypatch.setattr(llm_mod, "assess", boom)
    with pytest.raises(RuntimeError, match="transport blew up"):
        run_assessment(session)


def test_session_persists_report_after_assessment(monkeypatch: pytest.MonkeyPatch) -> None:
    session = InvestigationSession(ticket=_ticket())
    canned = LLMTriageOutput(
        finding="ok", confidence="low", evidence=[], suggested_note="n",
    )

    async def fake(_session, model=None, *, verbose=False):  # noqa: ARG001
        return canned

    from triage_cli import llm as llm_mod
    monkeypatch.setattr(llm_mod, "assess", fake)
    investigation.run_assessment(session)
    assert session.report is not None
    assert session.report.confidence == "low"
