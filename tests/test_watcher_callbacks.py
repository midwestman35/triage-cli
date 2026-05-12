"""Tests that watcher.run_iteration invokes optional callbacks in the right order."""
from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, call

import pytest

from triage_cli import watcher
from triage_cli.models import SiteEntry, Ticket, TimeWindow, TriageReport
from triage_cli.watcher import State, WatcherOptions


def _opts(tmp_path: Path) -> WatcherOptions:
    return WatcherOptions(
        view_id=99,
        interval=60,
        state_file=tmp_path / "state.json",
        backfill_hours=24.0,
        window_minutes=15,
        levels=["error", "warn"],
        no_logs=False,
        print_notes=False,
        verbose=False,
    )


def _make_ticket(tid: int, updated_at: datetime) -> Ticket:
    return Ticket(
        id=tid,
        subject="x",
        description="y",
        requester_org="Aurora 911, CO",
        tags=[],
        created_at=updated_at,
        updated_at=updated_at,
        comments=[],
    )


def _make_site() -> SiteEntry:
    return SiteEntry(
        friendly_name="Aurora 911, CO",
        site_name="us-co-aurora-apex",
        cnc="00000000-0000-0000-0000-000000000000",
    )


def _report(ticket_id: int, generated_at: datetime) -> TriageReport:
    return TriageReport(
        finding=f"finding for {ticket_id}",
        confidence="medium",
        evidence=[],
        suggested_note=f"note for {ticket_id}",
        ticket_id=ticket_id,
        site_name="us-co-aurora-apex",
        window=TimeWindow(start=generated_at, end=generated_at),
        sources=["zendesk"],
        log_event_count=0,
        generated_at=generated_at,
    )


def _zd_with_tickets(tickets: dict[int, Ticket], view_ids: list[int]) -> MagicMock:
    zd = MagicMock()
    zd.list_view_ticket_ids.return_value = view_ids
    zd.get_ticket.side_effect = lambda tid: tickets[tid]
    return zd


def test_run_iteration_no_callbacks_preserves_state_update(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """When no callbacks are passed, run_iteration behaves like before."""
    first = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    second = first + timedelta(minutes=5)
    cutoff = first - timedelta(hours=1)
    tickets = {
        101: _make_ticket(101, first),
        102: _make_ticket(102, second),
    }
    zd = _zd_with_tickets(tickets, [101, 102])

    monkeypatch.setattr(
        "triage_cli.pipeline.investigate_one",
        AsyncMock(side_effect=lambda ticket, **_kw: _report(ticket.id, ticket.updated_at)),
    )

    state: State = {"version": watcher.STATE_VERSION, "triaged": {}}
    new_state = watcher.run_iteration(
        zd,
        [_make_site()],
        state,
        _opts(tmp_path),
        cutoff,
        dd_client=None,
    )

    assert new_state["triaged"] == {
        "101": first.isoformat(),
        "102": second.isoformat(),
    }


def test_run_iteration_invokes_callbacks_in_order(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """on_view_listed -> on_progress -> on_complete fire in the right order."""
    first = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    second = first + timedelta(minutes=5)
    cutoff = first - timedelta(hours=1)
    reports = {
        101: _report(101, first),
        102: _report(102, second),
    }
    tickets = {
        101: _make_ticket(101, first),
        102: _make_ticket(102, second),
    }
    zd = _zd_with_tickets(tickets, [101, 102])

    monkeypatch.setattr(
        "triage_cli.pipeline.investigate_one",
        AsyncMock(side_effect=lambda ticket, **_kw: reports[ticket.id]),
    )

    callbacks = MagicMock()
    state: State = {"version": watcher.STATE_VERSION, "triaged": {}}
    watcher.run_iteration(
        zd,
        [_make_site()],
        state,
        _opts(tmp_path),
        cutoff,
        dd_client=None,
        on_view_listed=callbacks.on_view_listed,
        on_progress=callbacks.on_progress,
        on_complete=callbacks.on_complete,
    )

    assert callbacks.mock_calls == [
        call.on_view_listed([101, 102]),
        call.on_progress(101, "triaging"),
        call.on_complete(reports[101]),
        call.on_progress(102, "triaging"),
        call.on_complete(reports[102]),
    ]


def test_run_iteration_invokes_failure_callback(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """on_failure fires for a per-ticket failure without marking the ticket triaged."""
    now = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    cutoff = now - timedelta(hours=1)
    ticket = _make_ticket(101, now)
    zd = _zd_with_tickets({101: ticket}, [101])

    monkeypatch.setattr(
        "triage_cli.pipeline.investigate_one",
        AsyncMock(side_effect=RuntimeError("Datadog timeout")),
    )

    callbacks = MagicMock()
    state: State = {"version": watcher.STATE_VERSION, "triaged": {}}
    new_state = watcher.run_iteration(
        zd,
        [_make_site()],
        state,
        _opts(tmp_path),
        cutoff,
        dd_client=None,
        on_view_listed=callbacks.on_view_listed,
        on_progress=callbacks.on_progress,
        on_complete=callbacks.on_complete,
        on_failure=callbacks.on_failure,
    )

    assert new_state["triaged"] == {}
    assert callbacks.mock_calls == [
        call.on_view_listed([101]),
        call.on_progress(101, "triaging"),
        call.on_failure(101, "Datadog timeout"),
    ]


def test_run_iteration_failure_callback_for_get_ticket_error(
    tmp_path: Path,
) -> None:
    now = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    cutoff = now - timedelta(hours=1)
    zd = MagicMock()
    zd.list_view_ticket_ids.return_value = [101]
    zd.get_ticket.side_effect = RuntimeError("Zendesk unavailable")

    callbacks = MagicMock()
    state: State = {"version": watcher.STATE_VERSION, "triaged": {}}
    watcher.run_iteration(
        zd,
        [_make_site()],
        state,
        _opts(tmp_path),
        cutoff,
        dd_client=None,
        on_view_listed=callbacks.on_view_listed,
        on_failure=callbacks.on_failure,
    )

    assert callbacks.mock_calls == [
        call.on_view_listed([101]),
        call.on_failure(101, "Zendesk unavailable"),
    ]


def test_run_iteration_failure_callback_for_save_error(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """investigate_one raising OSError is treated as a per-ticket failure."""
    now = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    cutoff = now - timedelta(hours=1)
    ticket = _make_ticket(101, now)
    zd = _zd_with_tickets({101: ticket}, [101])

    monkeypatch.setattr(
        "triage_cli.pipeline.investigate_one",
        AsyncMock(side_effect=OSError("disk full")),
    )

    callbacks = MagicMock()
    state: State = {"version": watcher.STATE_VERSION, "triaged": {}}
    watcher.run_iteration(
        zd,
        [_make_site()],
        state,
        _opts(tmp_path),
        cutoff,
        dd_client=None,
        on_view_listed=callbacks.on_view_listed,
        on_progress=callbacks.on_progress,
        on_failure=callbacks.on_failure,
    )

    assert callbacks.mock_calls == [
        call.on_view_listed([101]),
        call.on_progress(101, "triaging"),
        call.on_failure(101, "disk full"),
    ]
