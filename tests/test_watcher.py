"""Tests for triage_cli.watcher."""
from __future__ import annotations

from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from triage_cli.models import Ticket
from triage_cli.watcher import (
    WatcherOptions,
    load_state,
    prune_state,
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
