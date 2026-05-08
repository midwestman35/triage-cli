"""Smoke tests for the Textual inbox app skeleton."""
from __future__ import annotations

import asyncio
from datetime import UTC, datetime, timedelta
from pathlib import Path

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
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path)
        async with app.run_test() as pilot:
            table = app.query_one("#list", TicketListWidget)
            assert table.row_count == 1
            assert "1 hydrated" in app.sub_title

            await pilot.press("down")
            detail = app.query_one("#detail", ReportPaneWidget)
            assert detail.current_report == report

    asyncio.run(run())


def test_inbox_app_q_quits(tmp_path: Path) -> None:
    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path)
        async with app.run_test() as pilot:
            await pilot.press("q")
            assert not app.is_running

    asyncio.run(run())
