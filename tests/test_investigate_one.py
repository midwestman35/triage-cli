"""Tests for pipeline.investigate_one."""
from __future__ import annotations

from datetime import UTC, datetime
from unittest.mock import AsyncMock


def _make_ticket(ticket_id: int = 1):
    from triage_cli.models import Ticket
    return Ticket(
        id=ticket_id,
        subject="SBC jitter on PSAP-01",
        description="calls dropping after 30s",
        created_at=datetime(2026, 5, 10, tzinfo=UTC),
        requester_email="ops@acme.com",
        comments=[],
    )


def _make_session(ticket):
    from triage_cli.investigation import create_session
    return create_session(ticket)


def _make_llm_output():
    from triage_cli.models import LLMTriageOutput
    return LLMTriageOutput(
        finding="test",
        confidence="medium",
        evidence=[],
        suggested_note="note",
        next_checks=[],
        unknowns=[],
    )


def test_investigate_one_returns_triage_report(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("LLM_PROVIDER", "claude")

    ticket = _make_ticket()
    session = _make_session(ticket)

    from triage_cli import llm
    monkeypatch.setattr(llm, "triage", AsyncMock(return_value=_make_llm_output()))

    import asyncio

    from triage_cli.pipeline import SilentReporter, investigate_one
    result = asyncio.run(
        investigate_one(
            ticket,
            session=session,
            reporter=SilentReporter(),
            interactive=False,
        )
    )
    from triage_cli.models import TriageReport
    assert isinstance(result, TriageReport)


def test_investigate_one_no_llm_stub(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    ticket = _make_ticket()
    session = _make_session(ticket)

    import asyncio

    from triage_cli.pipeline import SilentReporter, investigate_one
    result = asyncio.run(
        investigate_one(
            ticket,
            session=session,
            reporter=SilentReporter(),
            interactive=False,
            no_llm=True,
        )
    )
    assert "[stub]" in result.finding


def test_investigate_one_appends_to_memory(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("LLM_PROVIDER", "claude")
    ticket = _make_ticket()
    session = _make_session(ticket)

    from triage_cli import llm
    monkeypatch.setattr(llm, "triage", AsyncMock(return_value=_make_llm_output()))

    import asyncio

    from triage_cli.pipeline import SilentReporter, investigate_one
    asyncio.run(
        investigate_one(
            ticket,
            session=session,
            reporter=SilentReporter(),
            interactive=False,
        )
    )

    memory_md = tmp_path / "MEMORY.md"
    assert memory_md.exists()
    assert str(ticket.id) in memory_md.read_text()


def test_investigate_one_reporter_phases_called(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("LLM_PROVIDER", "claude")
    ticket = _make_ticket()
    session = _make_session(ticket)

    from triage_cli import llm
    monkeypatch.setattr(llm, "triage", AsyncMock(return_value=_make_llm_output()))

    phases_started = []
    phases_done = []

    class TrackingReporter:
        def phase_started(self, phase, detail=""): phases_started.append(phase)
        def phase_done(self, phase, detail=""): phases_done.append(phase)
        def phase_failed(self, phase, err): pass
        def evidence_added(self, item): pass
        def done(self, report): pass

    import asyncio

    from triage_cli.pipeline import investigate_one
    asyncio.run(
        investigate_one(
            ticket,
            session=session,
            reporter=TrackingReporter(),
            interactive=False,
        )
    )

    assert "customer_history" in phases_started or "customer_history" in phases_done
    assert "memory_lookup" in phases_started or "memory_lookup" in phases_done
    assert "llm_call" in phases_started or "llm_call" in phases_done
