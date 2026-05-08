"""Tests for inbox state model (RowEntry sort order, status transitions)."""
from __future__ import annotations

from datetime import UTC, datetime, timedelta

from triage_cli.inbox.widgets import RowEntry, sort_rows
from triage_cli.models import TimeWindow, TriageReport


def _report(ticket_id: int, age_hours: float = 1.0) -> TriageReport:
    when = datetime.now(UTC) - timedelta(hours=age_hours)
    return TriageReport(
        finding="x",
        confidence="low",
        evidence=[],
        suggested_note="y",
        ticket_id=ticket_id,
        site_name="x",
        window=TimeWindow(start=when, end=when),
        sources=["zendesk"],
        log_event_count=0,
        generated_at=when,
    )


def test_sort_triaging_above_triaged():
    rows = [
        RowEntry(ticket_id=1, status="triaged", report=_report(1, age_hours=1)),
        RowEntry(ticket_id=2, status="triaging", report=None),
    ]
    out = sort_rows(rows)
    assert out[0].ticket_id == 2


def test_sort_triaged_newest_first():
    rows = [
        RowEntry(ticket_id=1, status="triaged", report=_report(1, age_hours=3)),
        RowEntry(ticket_id=2, status="triaged", report=_report(2, age_hours=1)),
        RowEntry(ticket_id=3, status="triaged", report=_report(3, age_hours=2)),
    ]
    out = sort_rows(rows)
    assert [r.ticket_id for r in out] == [2, 3, 1]


def test_sort_pending_below_triaged_above_failed():
    rows = [
        RowEntry(ticket_id=1, status="failed", report=None, failure_reason="x"),
        RowEntry(ticket_id=2, status="triaged", report=_report(2)),
        RowEntry(ticket_id=3, status="pending", report=None),
    ]
    out = sort_rows(rows)
    assert [r.status for r in out] == ["triaged", "pending", "failed"]
