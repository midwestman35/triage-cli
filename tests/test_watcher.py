"""Tests for triage_cli.watcher."""
from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest

from triage_cli.models import SiteEntry, Ticket, TimeWindow, TriageReport
from triage_cli.watcher import (
    State,
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


def _report(ticket_id: int, generated_at: datetime) -> TriageReport:
    return TriageReport(
        finding=f"note for {ticket_id}",
        confidence="medium",
        evidence=[],
        suggested_note=f"suggested note for {ticket_id}",
        ticket_id=ticket_id,
        site_name="us-co-aurora-apex",
        window=TimeWindow(start=generated_at, end=generated_at),
        sources=["zendesk"],
        log_event_count=0,
        generated_at=generated_at,
    )


def test_load_state_returns_empty_default_when_missing(tmp_path: Path) -> None:
    state = load_state(tmp_path / "missing.json")
    assert state.version == 2
    assert state.triaged == {}


def test_load_state_round_trips_save_state(tmp_path: Path) -> None:
    path = tmp_path / "state.json"
    original = State(version=2, triaged={"42": "2026-05-07T12:00:00+00:00"})
    save_state(path, original)
    loaded = load_state(path)
    assert loaded.version == 2
    assert loaded.triaged == {"42": "2026-05-07T12:00:00+00:00"}


def test_save_state_atomic_no_temp_left_behind(tmp_path: Path) -> None:
    path = tmp_path / "state.json"
    save_state(path, State(version=2, triaged={}))
    assert path.exists()
    assert not (path.parent / (path.name + ".tmp")).exists()


def test_load_state_rejects_unknown_version(tmp_path: Path) -> None:
    path = tmp_path / "state.json"
    path.write_text('{"version": 99, "triaged": {}}', encoding="utf-8")
    with pytest.raises(RuntimeError, match="99"):
        load_state(path)


def test_should_triage_true_when_absent_and_within_cutoff() -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    cutoff = now - timedelta(hours=24)
    state = State(version=2, triaged={})
    assert should_triage(_ticket(42, now), state, cutoff) is True


def test_should_triage_false_when_absent_but_older_than_cutoff() -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    cutoff = now - timedelta(hours=1)
    older = now - timedelta(hours=2)
    state = State(version=2, triaged={})
    assert should_triage(_ticket(42, older), state, cutoff) is False


def test_should_triage_false_when_state_matches_updated_at() -> None:
    when = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    cutoff = when - timedelta(hours=24)
    state = State(version=2, triaged={"42": when.isoformat()})
    assert should_triage(_ticket(42, when), state, cutoff) is False


def test_should_triage_true_when_ticket_newer_than_state() -> None:
    earlier = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    later = earlier + timedelta(minutes=5)
    cutoff = earlier - timedelta(hours=24)
    state = State(version=2, triaged={"42": earlier.isoformat()})
    assert should_triage(_ticket(42, later), state, cutoff) is True


def test_prune_state_keeps_n_most_recent() -> None:
    triaged = {
        "1": "2026-05-01T12:00:00+00:00",
        "2": "2026-05-02T12:00:00+00:00",
        "3": "2026-05-03T12:00:00+00:00",
        "4": "2026-05-04T12:00:00+00:00",
    }
    state = State(version=2, triaged=triaged)
    pruned = prune_state(state, max_entries=2)
    assert set(pruned.triaged.keys()) == {"3", "4"}


def test_should_triage_handles_naive_stored_timestamp() -> None:
    """A naive ISO timestamp in state should still produce a usable comparison."""
    when = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    cutoff = when - timedelta(hours=24)
    # Naive (no offset) — externally edited / older format.
    state = State(version=2, triaged={"42": "2026-05-07T12:00:00"})
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
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
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
        return _report(ticket.id, now)

    monkeypatch.setattr("triage_cli.pipeline.triage_one", fake_triage_one)
    monkeypatch.setattr(
        "triage_cli.render.save_note",
        lambda report, tid: (tmp_path / f"{tid}.md", tmp_path / f"{tid}.json"),
    )

    state = State(version=2, triaged={})
    opts = _opts(tmp_path / "state.json")
    new_state = run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)

    assert new_state.triaged == {"1": now.isoformat()}


def test_run_iteration_status_lines(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture,
    stub_sites: list[SiteEntry],
) -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
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
        return _report(ticket.id, now)

    monkeypatch.setattr("triage_cli.pipeline.triage_one", fake_triage_one)
    monkeypatch.setattr(
        "triage_cli.render.save_note",
        lambda report, tid: (tmp_path / f"{tid}.md", tmp_path / f"{tid}.json"),
    )

    state = State(version=2, triaged={"4": now.isoformat()})
    opts = WatcherOptions(
        view_id=1,
        interval=300,
        state_file=tmp_path / "state.json",
        backfill_hours=24.0,
        window_minutes=30,
        levels=["error", "warn"],
        no_logs=False,
        print_notes=False,
        verbose=True,
    )
    run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)
    err = capsys.readouterr().err

    assert "#1 triaged" in err
    assert "#1 confidence: medium; events: 0; sources: zendesk" in err
    assert "#2 failed" in err and "Datadog timeout" in err and "will retry" in err
    assert "#3 skipped: site unresolvable" in err
    assert "#4 unchanged" in err


def test_run_iteration_print_notes_uses_rendered_markdown(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture,
    stub_sites: list[SiteEntry],
) -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    cutoff = now - timedelta(hours=24)
    ticket = Ticket(
        id=1,
        subject="s",
        description="us-co-aurora-apex",
        requester_org=None,
        tags=[],
        created_at=now,
        updated_at=now,
        comments=[],
    )
    zd = _zd_with_tickets({1: ticket}, [1])
    report = _report(1, now)

    monkeypatch.setattr("triage_cli.pipeline.triage_one", lambda *_args, **_kwargs: report)
    monkeypatch.setattr(
        "triage_cli.render.save_note",
        lambda report, tid: (tmp_path / f"{tid}.md", tmp_path / f"{tid}.json"),
    )
    monkeypatch.setattr(
        "triage_cli.render.to_markdown",
        lambda actual_report: "rendered note" if actual_report is report else "wrong report",
        raising=False,
    )

    state = State(version=2, triaged={})
    opts = WatcherOptions(
        view_id=1,
        interval=300,
        state_file=tmp_path / "state.json",
        backfill_hours=24.0,
        window_minutes=30,
        levels=["error", "warn"],
        no_logs=False,
        print_notes=True,
        verbose=False,
    )
    run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)

    out = capsys.readouterr().out
    assert out == "rendered note\n---\n"


def test_run_iteration_silent_backfill_marks_old_tickets_no_status(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture,
    stub_sites: list[SiteEntry],
) -> None:
    """A ticket older than the cutoff with no prior state entry is silently marked."""
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
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

    state = State(version=2, triaged={})
    opts = _opts(tmp_path / "state.json")
    new_state = run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)

    assert new_state.triaged == {"99": older.isoformat()}
    err = capsys.readouterr().err
    # No status line for ticket 99 — silent backfill.
    assert "#99" not in err


def test_run_watch_saves_state_on_keyboard_interrupt(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """run_watch saves state and exits cleanly on Ctrl-C."""
    saved: list[Any] = []

    def fake_run_iteration(zd, sites, state, opts, cutoff, dd_client):  # noqa: ARG001
        # Mutate state.triaged in place, then signal stop.
        state.triaged["111"] = "2026-05-07T12:00:00+00:00"
        raise KeyboardInterrupt

    def fake_save_state(path: Path, state: Any) -> None:
        saved.append((path, state))

    # Monkeypatch the site map load so we don't need a real cnc-map.json.
    monkeypatch.setattr("triage_cli.extract.load_site_map", lambda _path: [])
    monkeypatch.setattr("triage_cli.watcher.run_iteration", fake_run_iteration)
    monkeypatch.setattr("triage_cli.watcher.save_state", fake_save_state)
    # Avoid actually opening Zendesk/Datadog clients.
    monkeypatch.setattr(
        "triage_cli.watcher.ZendeskClient.from_env",
        classmethod(lambda cls: _DummyCM()),
    )
    monkeypatch.setattr(
        "triage_cli.watcher.DatadogClient.from_env",
        classmethod(lambda cls: _DummyCM()),
    )

    from triage_cli.watcher import run_watch

    state_path = tmp_path / "state.json"
    opts = WatcherOptions(
        view_id=1,
        interval=300,
        state_file=state_path,
        backfill_hours=24.0,
        window_minutes=30,
        levels=["error"],
        no_logs=True,  # Skip the DatadogClient context.
        print_notes=False,
        verbose=False,
    )

    # run_watch catches KeyboardInterrupt internally and returns cleanly.
    run_watch(opts)

    # save_state was called at least once and the saved state has the
    # mutation from fake_run_iteration.
    assert any("111" in s.triaged for _, s in saved)


def test_run_iteration_silent_backfill_does_not_double_triage(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture,
    stub_sites: list[SiteEntry],
) -> None:
    """A silent-backfilled ticket is not re-triaged on the next iteration."""
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    cutoff = now - timedelta(hours=1)
    older = now - timedelta(hours=2)

    t_old = Ticket(
        id=99, subject="s", description="us-co-aurora-apex", requester_org=None,
        tags=[], created_at=older, updated_at=older, comments=[],
    )
    zd = _zd_with_tickets({99: t_old}, [99])

    triage_calls: list[int] = []

    def fake_triage_one(ticket, site_entry, **kwargs):  # noqa: ARG001
        triage_calls.append(ticket.id)
        return _report(ticket.id, older)

    monkeypatch.setattr("triage_cli.pipeline.triage_one", fake_triage_one)
    monkeypatch.setattr(
        "triage_cli.render.save_note",
        lambda report, tid: (tmp_path / f"{tid}.md", tmp_path / f"{tid}.json"),
    )

    state = State(version=2, triaged={})
    opts = _opts(tmp_path / "state.json")

    # First iteration: backfill-silent, no triage call.
    state = run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)
    assert triage_calls == []
    assert state.triaged == {"99": older.isoformat()}

    # Second iteration: same ticket, same updated_at — should be unchanged, no triage.
    state = run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)
    assert triage_calls == []
    err = capsys.readouterr().err
    # Second iteration should now emit "unchanged" since prior entry exists.
    assert "#99 unchanged" in err


def test_state_migrates_v1_to_v2_on_read(tmp_path) -> None:
    """A v1 state file should be readable; the read should populate ui.density."""
    import json

    from triage_cli.watcher import load_state

    v1_file = tmp_path / "watcher-state-test.json"
    v1_file.write_text(json.dumps({
        "version": 1,
        "triaged": {"123": "2026-05-09T12:00:00+00:00"},
    }))

    state = load_state(v1_file)
    assert state.version == 2
    assert state.ui is not None
    assert state.ui.density == "comfortable"
    assert state.triaged == {"123": "2026-05-09T12:00:00+00:00"}


def test_state_density_round_trip(tmp_path) -> None:
    """Writing then reading a v2 state file preserves ui.density."""
    from triage_cli.watcher import WatcherUIState, load_state, save_state

    v2_state = State(
        version=2,
        triaged={"5": "2026-05-09T12:00:00+00:00"},
        ui=WatcherUIState(density="compact"),
    )
    target = tmp_path / "watcher-state-test.json"
    save_state(target, v2_state)

    loaded = load_state(target)
    assert loaded.ui is not None
    assert loaded.ui.density == "compact"


class _DummyCM:
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False
