"""Smoke tests for the Textual inbox app skeleton."""
from __future__ import annotations

import asyncio
import sys
import threading
from datetime import UTC, datetime, timedelta
from pathlib import Path

from textual.widgets import Static

import triage_cli.inbox.app as app_module
from triage_cli.inbox.app import InboxApp
from triage_cli.inbox.widgets import ReportPaneWidget, TicketListWidget
from triage_cli.models import TimeWindow, TriageReport
from triage_cli.watcher import WatcherOptions


def _opts(tmp_path: Path) -> WatcherOptions:
    return WatcherOptions(
        view_id=99,
        interval=60,
        state_file=tmp_path / "state.json",
        backfill_hours=0.0,
        window_minutes=15,
        levels=["error", "warn"],
        no_logs=False,
        print_notes=False,
        verbose=False,
    )


def _write_report(notes_dir: Path, ticket_id: int, generated_at: datetime) -> TriageReport:
    report = TriageReport(
        finding=f"finding {ticket_id}",
        confidence="medium",
        evidence=[],
        suggested_note=f"note {ticket_id}",
        ticket_id=ticket_id,
        site_name="us-co-aurora-apex",
        window=TimeWindow(start=generated_at, end=generated_at),
        sources=["zendesk"],
        log_event_count=0,
        generated_at=generated_at,
    )
    notes_dir.mkdir(parents=True, exist_ok=True)
    (notes_dir / f"{ticket_id}-{generated_at:%Y%m%dT%H%M%SZ}.json").write_text(
        report.model_dump_json(),
        encoding="utf-8",
    )
    return report


def test_inbox_app_hydrates_and_updates_detail(tmp_path: Path) -> None:
    now = datetime.now(UTC)
    report = _write_report(tmp_path, 101, now - timedelta(minutes=5))

    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test() as pilot:
            table = app.query_one("#list", TicketListWidget)
            assert table.row_count == 1
            assert app.sub_title == "99 - 1 ticket - last poll: -"

            await pilot.press("down")
            detail = app.query_one("#detail", ReportPaneWidget)
            assert detail.current_report == report

    asyncio.run(run())


def test_inbox_app_empty_state_subtitle_and_detail_copy(tmp_path: Path) -> None:
    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test():
            table = app.query_one("#list", TicketListWidget)
            assert table.row_count == 0
            assert app.sub_title == "99 - no tickets - last poll: -"

            detail = app.query_one("#detail", ReportPaneWidget)
            assert detail.current_report is None
            report_body = detail.query_one("#report-body", Static)
            assert "Select a ticket" in str(report_body.content)

    asyncio.run(run())


def test_inbox_app_focus_actions_move_between_panes(tmp_path: Path) -> None:
    _write_report(tmp_path, 101, datetime.now(UTC) - timedelta(minutes=5))

    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test() as pilot:
            table = app.query_one("#list", TicketListWidget)
            detail = app.query_one("#detail", ReportPaneWidget)

            await pilot.press("enter")
            assert app.focused == detail

            await pilot.press("escape")
            assert app.focused == table

    asyncio.run(run())


def test_inbox_app_refresh_rehydrates_from_disk(tmp_path: Path) -> None:
    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test() as pilot:
            table = app.query_one("#list", TicketListWidget)
            assert table.row_count == 0

            _write_report(tmp_path, 202, datetime.now(UTC) - timedelta(minutes=5))
            await pilot.press("r")
            await pilot.pause()

            assert table.row_count == 1
            assert "1 ticket" in app.sub_title

    asyncio.run(run())


def test_inbox_app_copy_note_uses_selected_report(
    tmp_path: Path,
    monkeypatch,
) -> None:
    report = _write_report(tmp_path, 101, datetime.now(UTC) - timedelta(minutes=5))
    copied: list[str] = []

    def copy_to_clipboard(text: str) -> bool:
        copied.append(text)
        return True

    monkeypatch.setattr(
        "triage_cli.inbox.app.clipboard.copy_to_clipboard",
        copy_to_clipboard,
    )

    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test() as pilot:
            await pilot.press("down")
            await pilot.press("y")

    asyncio.run(run())

    assert copied == [report.suggested_note]


def test_inbox_app_open_zendesk_uses_selected_ticket(
    tmp_path: Path,
    monkeypatch,
) -> None:
    _write_report(tmp_path, 101, datetime.now(UTC) - timedelta(minutes=5))
    opened: list[str] = []
    monkeypatch.setenv("ZENDESK_SUBDOMAIN", "example")

    def open_url(url: str) -> bool:
        opened.append(url)
        return True

    monkeypatch.setattr("triage_cli.inbox.app.webbrowser.open", open_url)

    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test() as pilot:
            await pilot.press("down")
            await pilot.press("o")

    asyncio.run(run())

    assert opened == ["https://example.zendesk.com/agent/tickets/101"]


def test_inbox_app_q_quits(tmp_path: Path) -> None:
    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test() as pilot:
            await pilot.press("q")
            assert not app.is_running

    asyncio.run(run())


def test_reconcile_and_status_callbacks_update_rows(tmp_path: Path) -> None:
    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test() as pilot:
            app._reconcile_pending({101, 202})
            assert app._rows[101].status == "queued"
            assert app._rows[202].status == "queued"

            app._set_status(101, "triaging")
            assert app._rows[101].status == "triaging"

            app._reconcile_pending({101})
            assert 202 not in app._rows

            app._set_failure(303, "boom")
            assert app._rows[303].status == "failed"
            assert app._rows[303].failure_reason == "boom"

            report = _write_report(tmp_path, 101, datetime.now(UTC))
            app._add_or_update_report(report)
            assert app._rows[101].status == "triaged"
            assert app._rows[101].report == report
            await pilot.pause()

    asyncio.run(run())


def test_selected_row_icon_marks_cursor_row(tmp_path: Path) -> None:
    """The selected row's icon column carries the ◉ marker; other rows do not."""
    now = datetime.now(UTC)
    _write_report(tmp_path, 1, now - timedelta(hours=2))
    _write_report(tmp_path, 2, now - timedelta(minutes=5))

    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test() as pilot:
            table = app.query_one("#list", TicketListWidget)
            assert table.row_count == 2
            # Newest first: row 0 = #2, row 1 = #1. Cursor starts at row 0 (#2).
            assert "◉" in table.get_cell_at((0, 0))
            assert "◉" not in table.get_cell_at((1, 0))
            # Move cursor to #1 and re-sort; the marker should follow.
            await pilot.press("down")
            app._refresh_list()
            assert "◉" in table.get_cell_at((1, 0))
            assert "◉" not in table.get_cell_at((0, 0))

    asyncio.run(run())


def test_refresh_preserves_selected_ticket_after_resort(tmp_path: Path) -> None:
    old_report = _write_report(tmp_path, 101, datetime.now(UTC) - timedelta(hours=2))
    new_report = _write_report(tmp_path, 202, datetime.now(UTC) - timedelta(minutes=5))

    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test() as pilot:
            table = app.query_one("#list", TicketListWidget)
            await pilot.press("down")
            await pilot.press("down")
            assert app._currently_selected().report == old_report

            app._set_status(new_report.ticket_id, "triaging")
            assert app._currently_selected().report == old_report
            assert table.cursor_row == 1

    asyncio.run(run())


def test_poll_tick_has_single_flight_guard(tmp_path: Path, monkeypatch) -> None:
    started = threading.Event()
    release = threading.Event()
    calls = 0

    def run_iteration_blocking(_app: InboxApp, state):
        nonlocal calls
        calls += 1
        started.set()
        assert release.wait(timeout=2)
        return state

    monkeypatch.setattr(InboxApp, "_run_iteration_blocking", run_iteration_blocking)

    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        first_poll = asyncio.create_task(app._poll_tick())
        assert await asyncio.to_thread(started.wait, 1)

        await app._poll_tick()
        assert calls == 1

        release.set()
        await asyncio.wait_for(first_poll, timeout=2)
        assert calls == 1
        assert app._polling is False
        assert app._last_poll is not None

    asyncio.run(run())


def test_poll_tick_assigns_state_on_event_loop(tmp_path: Path, monkeypatch) -> None:
    """Regression: ``self._state`` is reassigned by ``_poll_tick``, not by the worker.

    The worker must return the new state so that ``self._state`` is mutated only on
    the event-loop thread — preventing concurrent reads/writes if the single-flight
    guard is ever loosened.
    """
    seen_state_ids: list[int] = []

    def run_iteration_blocking(self: InboxApp, state):
        # Worker thread observes the state passed in; it must NOT mutate self._state itself.
        seen_state_ids.append(id(state))
        return {"version": 1, "triaged": {"abc": "ts"}}

    monkeypatch.setattr(InboxApp, "_run_iteration_blocking", run_iteration_blocking)

    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        original_state_id = id(app._state)
        await app._poll_tick()
        # The worker saw the original state object…
        assert seen_state_ids == [original_state_id]
        # …and after _poll_tick returns, the event loop has installed the new one.
        assert app._state == {"version": 1, "triaged": {"abc": "ts"}}

    asyncio.run(run())


def test_run_iteration_blocking_uses_watcher_dependencies(
    tmp_path: Path,
    monkeypatch,
) -> None:
    zd = object()
    dd = object()
    sites = [object()]
    captured: dict[str, object] = {}
    saved: dict[str, object] = {}

    class Context:
        def __init__(self, value: object) -> None:
            self.value = value

        def __enter__(self) -> object:
            return self.value

        def __exit__(self, *_exc: object) -> None:
            return None

    def load_site_map(path: Path) -> list[object]:
        captured["site_map_path"] = path
        return sites

    def run_iteration(
        zd_arg,
        sites_arg,
        state_arg,
        opts_arg,
        backfill_cutoff,
        dd_client,
        *,
        on_view_listed,
        on_progress,
        on_complete,
        on_failure,
    ):
        captured.update(
            {
                "zd": zd_arg,
                "sites": sites_arg,
                "state": state_arg,
                "opts": opts_arg,
                "backfill_cutoff": backfill_cutoff,
                "dd_client": dd_client,
                "callbacks": (
                    on_view_listed,
                    on_progress,
                    on_complete,
                    on_failure,
                ),
            },
        )
        on_view_listed([])  # must call so _run_iteration_blocking doesn't raise
        return {"version": 1, "triaged": {"123": "timestamp"}}

    def prune_state(state):
        captured["pruned_state"] = state
        return state

    def save_state(path: Path, state) -> None:
        saved["path"] = path
        saved["state"] = state

    monkeypatch.setattr(app_module.extract, "load_site_map", load_site_map)
    monkeypatch.setattr(app_module.ZendeskClient, "from_env", lambda: Context(zd))
    monkeypatch.setattr(app_module.DatadogClient, "from_env", lambda: Context(dd))
    monkeypatch.setattr(app_module.watcher, "run_iteration", run_iteration)
    monkeypatch.setattr(app_module.watcher, "prune_state", prune_state)
    monkeypatch.setattr(app_module.watcher, "save_state", save_state)

    opts = _opts(tmp_path)
    app = InboxApp(opts, notes_dir=tmp_path)
    new_state = app._run_iteration_blocking(app._state)

    assert captured["site_map_path"] == Path("data/cnc-map.json")
    assert captured["zd"] is zd
    assert captured["sites"] == sites
    assert captured["state"] == {"version": 1, "triaged": {}}
    assert captured["opts"] is opts
    assert captured["dd_client"] is dd
    assert captured["backfill_cutoff"].tzinfo is UTC
    assert all(callable(callback) for callback in captured["callbacks"])
    assert new_state == {"version": 1, "triaged": {"123": "timestamp"}}
    assert saved == {"path": opts.state_file, "state": new_state}
    assert captured["backfill_cutoff"] == app._backfill_cutoff


def test_run_iteration_blocking_keeps_stable_backfill_cutoff(
    tmp_path: Path,
    monkeypatch,
) -> None:
    cutoffs: list[datetime] = []

    class Context:
        def __enter__(self) -> object:
            return object()

        def __exit__(self, *_exc: object) -> None:
            return None

    def run_iteration(
        _zd,
        _sites,
        _state,
        _opts,
        backfill_cutoff,
        dd_client=None,
        *,
        on_view_listed,
        **_callbacks,
    ):
        cutoffs.append(backfill_cutoff)
        on_view_listed([])  # must call so _run_iteration_blocking doesn't raise
        return {"version": 1, "triaged": {}}

    monkeypatch.setattr(app_module.extract, "load_site_map", lambda _path: [])
    monkeypatch.setattr(app_module.ZendeskClient, "from_env", lambda: Context())
    monkeypatch.setattr(app_module.watcher, "run_iteration", run_iteration)
    monkeypatch.setattr(app_module.watcher, "save_state", lambda *_args: None)

    opts = WatcherOptions(
        view_id=99,
        interval=60,
        state_file=tmp_path / "state.json",
        backfill_hours=0.0,
        window_minutes=15,
        levels=["error", "warn"],
        no_logs=True,
        print_notes=False,
        verbose=False,
    )
    app = InboxApp(opts, notes_dir=tmp_path)
    app._run_iteration_blocking(app._state)
    app._run_iteration_blocking(app._state)

    assert len(cutoffs) == 2
    assert cutoffs[0] == cutoffs[1] == app._backfill_cutoff


def test_run_iteration_blocking_routes_watcher_stderr_to_log(
    tmp_path: Path,
    monkeypatch,
    capsys,
    caplog,
) -> None:
    class Context:
        def __enter__(self) -> object:
            return object()

        def __exit__(self, *_exc: object) -> None:
            return None

    def run_iteration(*_args, on_view_listed, **_kwargs):
        print("watcher status line", file=sys.stderr)
        on_view_listed([])  # must call so _run_iteration_blocking doesn't raise
        return {"version": 1, "triaged": {}}

    monkeypatch.setattr(app_module.extract, "load_site_map", lambda _path: [])
    monkeypatch.setattr(app_module.ZendeskClient, "from_env", lambda: Context())
    monkeypatch.setattr(app_module.watcher, "run_iteration", run_iteration)
    monkeypatch.setattr(app_module.watcher, "save_state", lambda *_args: None)

    app = InboxApp(
        WatcherOptions(
            view_id=99,
            interval=60,
            state_file=tmp_path / "state.json",
            backfill_hours=0.0,
            window_minutes=15,
            levels=["error", "warn"],
            no_logs=True,
            print_notes=False,
            verbose=False,
        ),
        notes_dir=tmp_path,
    )
    with caplog.at_level("DEBUG", logger="triage_cli.inbox.app"):
        app._run_iteration_blocking(app._state)

    assert "watcher status line" in caplog.text
    assert "watcher status line" not in capsys.readouterr().err


def test_run_iteration_blocking_logs_iteration_aborted_at_warning(
    tmp_path: Path,
    monkeypatch,
    caplog,
) -> None:
    """'iteration aborted' lines from watcher are logged at WARNING, not DEBUG."""

    class Context:
        def __enter__(self) -> object:
            return object()

        def __exit__(self, *_exc: object) -> None:
            return None

    def run_iteration(*_args, on_view_listed=None, **_kwargs):
        if on_view_listed is not None:
            on_view_listed([99])
        print("[17:00:00] iteration aborted: Zendesk error 400", file=sys.stderr)
        print("[17:00:00] #12345 unchanged", file=sys.stderr)
        return {"version": 1, "triaged": {}}

    monkeypatch.setattr(app_module.extract, "load_site_map", lambda _path: [])
    monkeypatch.setattr(app_module.ZendeskClient, "from_env", lambda: Context())
    monkeypatch.setattr(app_module.watcher, "run_iteration", run_iteration)
    monkeypatch.setattr(app_module.watcher, "save_state", lambda *_args: None)

    inbox = InboxApp(
        WatcherOptions(
            view_id=99,
            interval=60,
            state_file=tmp_path / "state.json",
            backfill_hours=0.0,
            window_minutes=15,
            levels=["error", "warn"],
            no_logs=True,
            print_notes=False,
            verbose=False,
        ),
        notes_dir=tmp_path,
    )
    with caplog.at_level("DEBUG", logger="triage_cli.inbox.app"):
        inbox._run_iteration_blocking(inbox._state)

    warning_lines = [r.message for r in caplog.records if r.levelname == "WARNING"]

    assert any("iteration aborted" in m for m in warning_lines), (
        "'iteration aborted' must be logged at WARNING so it appears in the default log"
    )
    assert any("#12345 unchanged" in m for m in warning_lines), (
        "all watcher output lines must be logged at WARNING"
    )


def test_set_phase_updates_row_entry(tmp_path: Path) -> None:
    """_set_phase stores phase label and step on the RowEntry."""
    from triage_cli.inbox.widgets import RowEntry

    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test():
            app._rows[999] = RowEntry(ticket_id=999, status="triaging", report=None)
            app._set_phase(999, "Asking Claude", 4)
            await asyncio.sleep(0)

            entry = app._rows[999]
            assert entry.phase_label == "Asking Claude"
            assert entry.phase_step == 4

    asyncio.run(run())
