"""Tests for triage_cli.watcher."""
from __future__ import annotations

from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from triage_cli.models import SiteEntry, Ticket
from triage_cli.watcher import (
    WatcherOptions,
    load_state,
    prune_state,
    run_iteration,
    save_state,
    should_triage,
)


def _ticket(ticket_id: int, updated_at: datetime) -> Ticket:
    return Ticket(
        id=ticket_id,
        subject="x",
        description="x",
        requester_org=None,
        tags=[],
        created_at=updated_at,
        updated_at=updated_at,
        comments=[],
    )


def _opts(state_file: Path) -> WatcherOptions:
    return WatcherOptions(
        view_id=1,
        interval=300,
        state_file=state_file,
        backfill_hours=24.0,
        window_minutes=30,
        levels=["error", "warn"],
        no_logs=False,
        print_notes=False,
        verbose=False,
    )


def test_load_state_returns_empty_default_when_missing(tmp_path: Path) -> None:
    state = load_state(tmp_path / "missing.json")
    assert state == {"version": 1, "triaged": {}}


def test_load_state_round_trips_save_state(tmp_path: Path) -> None:
    path = tmp_path / "state.json"
    original = {"version": 1, "triaged": {"42": "2026-05-07T12:00:00+00:00"}}
    save_state(path, original)
    assert load_state(path) == original


def test_save_state_atomic_no_temp_left_behind(tmp_path: Path) -> None:
    path = tmp_path / "state.json"
    save_state(path, {"version": 1, "triaged": {}})
    assert path.exists()
    assert not (path.parent / (path.name + ".tmp")).exists()


def test_load_state_rejects_unknown_version(tmp_path: Path) -> None:
    path = tmp_path / "state.json"
    path.write_text('{"version": 99, "triaged": {}}', encoding="utf-8")
    with pytest.raises(RuntimeError, match="version 99"):
        load_state(path)


def test_should_triage_true_when_absent_and_within_cutoff() -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = now - timedelta(hours=24)
    state = {"version": 1, "triaged": {}}
    assert should_triage(_ticket(42, now), state, cutoff) is True


def test_should_triage_false_when_absent_but_older_than_cutoff() -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = now - timedelta(hours=1)
    older = now - timedelta(hours=2)
    state = {"version": 1, "triaged": {}}
    assert should_triage(_ticket(42, older), state, cutoff) is False


def test_should_triage_false_when_state_matches_updated_at() -> None:
    when = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = when - timedelta(hours=24)
    state = {"version": 1, "triaged": {"42": when.isoformat()}}
    assert should_triage(_ticket(42, when), state, cutoff) is False


def test_should_triage_true_when_ticket_newer_than_state() -> None:
    earlier = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    later = earlier + timedelta(minutes=5)
    cutoff = earlier - timedelta(hours=24)
    state = {"version": 1, "triaged": {"42": earlier.isoformat()}}
    assert should_triage(_ticket(42, later), state, cutoff) is True


def test_prune_state_keeps_n_most_recent() -> None:
    triaged = {
        "1": "2026-05-01T12:00:00+00:00",
        "2": "2026-05-02T12:00:00+00:00",
        "3": "2026-05-03T12:00:00+00:00",
        "4": "2026-05-04T12:00:00+00:00",
    }
    state = {"version": 1, "triaged": triaged}
    pruned = prune_state(state, max_entries=2)
    assert set(pruned["triaged"].keys()) == {"3", "4"}


def test_should_triage_handles_naive_stored_timestamp() -> None:
    """A naive ISO timestamp in state should still produce a usable comparison."""
    when = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = when - timedelta(hours=24)
    # Naive (no offset) — externally edited / older format.
    state = {"version": 1, "triaged": {"42": "2026-05-07T12:00:00"}}
    # Same wall time → not newer → should NOT triage.
    assert should_triage(_ticket(42, when), state, cutoff) is False
    # 5 minutes later → newer → SHOULD triage.
    assert should_triage(_ticket(42, when + timedelta(minutes=5)), state, cutoff) is True


@pytest.fixture
def stub_sites() -> list[SiteEntry]:
    return [
        SiteEntry(
            friendly_name="Aurora 911, CO",
            site_name="us-co-aurora-apex",
            cnc="921d7c53-e815-4566-9692-6cbce589e1d3",
        ),
    ]


def _zd_with_tickets(tickets: dict[int, Ticket], view_ids: list[int]) -> MagicMock:
    zd = MagicMock()
    zd.list_view_ticket_ids.return_value = view_ids
    zd.get_ticket.side_effect = lambda tid: tickets[tid]
    return zd


def test_run_iteration_marks_only_successfully_triaged(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    stub_sites: list[SiteEntry],
) -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = now - timedelta(hours=24)
    t_ok = Ticket(
        id=1, subject="s", description="us-co-aurora-apex", requester_org=None,
        tags=[], created_at=now, updated_at=now, comments=[],
    )
    t_fail = Ticket(
        id=2, subject="s", description="us-co-aurora-apex", requester_org=None,
        tags=[], created_at=now, updated_at=now, comments=[],
    )
    t_no_site = Ticket(
        id=3, subject="s", description="no match here", requester_org=None,
        tags=[], created_at=now, updated_at=now, comments=[],
    )
    zd = _zd_with_tickets({1: t_ok, 2: t_fail, 3: t_no_site}, [1, 2, 3])

    def fake_triage_one(ticket, site_entry, **kwargs):  # noqa: ARG001
        if ticket.id == 2:
            raise RuntimeError("simulated Datadog timeout")
        return f"## Summary\nnote for {ticket.id}\n"

    monkeypatch.setattr("triage_cli.pipeline.triage_one", fake_triage_one)
    monkeypatch.setattr("triage_cli.render.save_note", lambda md, tid: tmp_path / f"{tid}.md")

    state = {"version": 1, "triaged": {}}
    opts = _opts(tmp_path / "state.json")
    new_state = run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)

    assert new_state["triaged"] == {"1": now.isoformat()}


def test_run_iteration_status_lines(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture,
    stub_sites: list[SiteEntry],
) -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = now - timedelta(hours=24)
    tickets = {
        1: Ticket(id=1, subject="s", description="us-co-aurora-apex", requester_org=None,
                  tags=[], created_at=now, updated_at=now, comments=[]),
        2: Ticket(id=2, subject="s", description="us-co-aurora-apex", requester_org=None,
                  tags=[], created_at=now, updated_at=now, comments=[]),
        3: Ticket(id=3, subject="s", description="no match", requester_org=None,
                  tags=[], created_at=now, updated_at=now, comments=[]),
        4: Ticket(id=4, subject="s", description="us-co-aurora-apex", requester_org=None,
                  tags=[], created_at=now, updated_at=now, comments=[]),
    }
    zd = _zd_with_tickets(tickets, [1, 2, 3, 4])

    def fake_triage_one(ticket, site_entry, **kwargs):  # noqa: ARG001
        if ticket.id == 2:
            raise RuntimeError("Datadog timeout")
        return f"note {ticket.id}"

    monkeypatch.setattr("triage_cli.pipeline.triage_one", fake_triage_one)
    monkeypatch.setattr("triage_cli.render.save_note", lambda md, tid: tmp_path / f"{tid}.md")

    state = {"version": 1, "triaged": {"4": now.isoformat()}}
    opts = _opts(tmp_path / "state.json")
    run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)
    err = capsys.readouterr().err

    assert "#1 triaged" in err
    assert "#2 failed" in err and "Datadog timeout" in err and "will retry" in err
    assert "#3 skipped: site unresolvable" in err
    assert "#4 unchanged" in err


def test_run_iteration_silent_backfill_marks_old_tickets_no_status(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture,
    stub_sites: list[SiteEntry],
) -> None:
    """A ticket older than the cutoff with no prior state entry is silently marked."""
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = now - timedelta(hours=1)
    older = now - timedelta(hours=2)

    t_old = Ticket(
        id=99, subject="s", description="us-co-aurora-apex", requester_org=None,
        tags=[], created_at=older, updated_at=older, comments=[],
    )
    zd = _zd_with_tickets({99: t_old}, [99])

    # pipeline.triage_one should NOT be called for a backfill-skipped ticket.
    def boom(*_args, **_kwargs):
        raise AssertionError("triage_one should not be called for silent-backfill ticket")

    monkeypatch.setattr("triage_cli.pipeline.triage_one", boom)

    state = {"version": 1, "triaged": {}}
    opts = _opts(tmp_path / "state.json")
    new_state = run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)

    assert new_state["triaged"] == {"99": older.isoformat()}
    err = capsys.readouterr().err
    # No status line for ticket 99 — silent backfill.
    assert "#99" not in err
