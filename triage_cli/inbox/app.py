"""Inbox Textual app: vertical-split list/detail with disk hydration on mount."""
from __future__ import annotations

import asyncio
import contextlib
import io
import logging
import math
import os
import webbrowser
from datetime import UTC, datetime, timedelta
from pathlib import Path

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal
from textual.widgets import DataTable, Footer, Header

from triage_cli import extract, watcher
from triage_cli.datadog import DatadogClient
from triage_cli.inbox import clipboard, hydrate
from triage_cli.inbox.widgets import ReportPaneWidget, RowEntry, Status, TicketListWidget
from triage_cli.models import TriageReport
from triage_cli.render import DEFAULT_OUTPUT_DIR
from triage_cli.watcher import WatcherOptions
from triage_cli.zendesk import ZendeskClient

logger = logging.getLogger(__name__)


class InboxApp(App):
    """Vertical-split inbox: TicketListWidget left, ReportPaneWidget right."""

    BINDINGS = [
        Binding("up,k", "cursor_up", "up", priority=True),
        Binding("down,j", "cursor_down", "down", priority=True),
        Binding("enter", "focus_detail", "focus", priority=True),
        Binding("escape", "focus_list", "", priority=True),
        Binding("r", "refresh", "refresh", priority=True),
        Binding("y", "copy_note", "copy", priority=True),
        Binding("o", "open_zendesk", "open", priority=True),
        Binding("q,ctrl+c", "quit", "quit", priority=True),
    ]

    CSS = """
    Horizontal { height: 1fr; }
    TicketListWidget { width: 45%; min-width: 50; }
    ReportPaneWidget { width: 55%; }
    """

    TITLE = "triage-cli inbox"

    def __init__(
        self,
        opts: WatcherOptions,
        *,
        notes_dir: Path | None = None,
        poll_on_mount: bool = True,
    ):
        super().__init__()
        self.opts = opts
        self.notes_dir = notes_dir if notes_dir is not None else DEFAULT_OUTPUT_DIR
        self.poll_on_mount = poll_on_mount
        self._rows: dict[int, RowEntry] = {}
        self._polling = False
        self._view_ids: set[int] = set()
        self._state = watcher.load_state(opts.state_file)
        self._last_poll: datetime | None = None
        self._backfill_cutoff = (
            datetime.now(UTC) - timedelta(hours=self.opts.backfill_hours)
            if math.isfinite(self.opts.backfill_hours)
            else datetime.min.replace(tzinfo=UTC)
        )

    def compose(self) -> ComposeResult:
        yield Header()
        with Horizontal():
            yield TicketListWidget(id="list")
            yield ReportPaneWidget(id="detail")
        yield Footer()

    async def on_mount(self) -> None:
        self._hydrate_recent_reports()
        self._refresh_list()
        self.query_one("#detail", ReportPaneWidget).show(None)
        self.query_one("#list", TicketListWidget).focus()
        if self.poll_on_mount:
            self.set_interval(self.opts.interval, self._poll_tick)
            self.run_worker(self._poll_tick(), exclusive=False)

    def _hydrate_recent_reports(self) -> int:
        hydrated_count = 0
        for report in hydrate.recent_reports(self.notes_dir, hours=24):
            self._rows[report.ticket_id] = RowEntry(
                ticket_id=report.ticket_id,
                status="triaged",
                report=report,
            )
            hydrated_count += 1
        return hydrated_count

    def _refresh_list(self) -> None:
        list_widget = self.query_one("#list", TicketListWidget)
        selected_ticket_id = self._selected_ticket_id()
        list_widget.refresh_rows(list(self._rows.values()), selected_ticket_id=selected_ticket_id)
        last_poll = self._last_poll.strftime("%H:%M") if self._last_poll else "-"
        ticket_count = len(self._rows)
        if ticket_count == 0:
            self.sub_title = f"view {self.opts.view_id} - no tickets - last poll: {last_poll}"
        else:
            ticket_word = "ticket" if ticket_count == 1 else "tickets"
            self.sub_title = (
                f"view {self.opts.view_id} - {ticket_count} {ticket_word} - "
                f"last poll: {last_poll}"
            )

    async def _poll_tick(self) -> None:
        if self._polling:
            if self.opts.verbose:
                self.notify(
                    "Previous poll still running",
                    severity="information",
                    timeout=2,
                )
            return

        self._polling = True
        try:
            await asyncio.to_thread(self._run_iteration_blocking)
        finally:
            self._polling = False
            self._last_poll = datetime.now(UTC)
            if self.is_running:
                self._refresh_list()

    def _run_iteration_blocking(self) -> None:
        """Run one watcher iteration in a worker thread."""
        sites = extract.load_site_map(Path("data/cnc-map.json"))

        stderr_buffer = io.StringIO()
        with contextlib.redirect_stderr(stderr_buffer), ZendeskClient.from_env() as zd:
            if self.opts.no_logs:
                new_state = watcher.run_iteration(
                    zd,
                    sites,
                    self._state,
                    self.opts,
                    self._backfill_cutoff,
                    dd_client=None,
                    on_view_listed=self._on_view_listed,
                    on_progress=self._on_progress,
                    on_complete=self._on_complete,
                    on_failure=self._on_failure,
                )
            else:
                with DatadogClient.from_env() as dd:
                    new_state = watcher.run_iteration(
                        zd,
                        sites,
                        self._state,
                        self.opts,
                        self._backfill_cutoff,
                        dd_client=dd,
                        on_view_listed=self._on_view_listed,
                        on_progress=self._on_progress,
                        on_complete=self._on_complete,
                        on_failure=self._on_failure,
                    )

        for line in stderr_buffer.getvalue().splitlines():
            logger.warning(line)

        self._state = new_state
        watcher.save_state(self.opts.state_file, watcher.prune_state(new_state))

    def _on_view_listed(self, view_ids: list[int]) -> None:
        self.call_from_thread(self._reconcile_pending, set(view_ids))

    def _on_progress(self, ticket_id: int, _status: str) -> None:
        self.call_from_thread(self._set_status, ticket_id, "triaging")

    def _on_complete(self, report: TriageReport) -> None:
        self.call_from_thread(self._add_or_update_report, report)

    def _on_failure(self, ticket_id: int, error: str) -> None:
        self.call_from_thread(self._set_failure, ticket_id, error)

    def _reconcile_pending(self, view_ids: set[int]) -> None:
        """Add pending rows for view tickets, and remove stale pending rows."""
        self._view_ids = view_ids
        for ticket_id in self._view_ids:
            if ticket_id not in self._rows:
                self._rows[ticket_id] = RowEntry(
                    ticket_id=ticket_id,
                    status="pending",
                    report=None,
                )

        for ticket_id in list(self._rows):
            if self._rows[ticket_id].status == "pending" and ticket_id not in self._view_ids:
                del self._rows[ticket_id]

        self._refresh_list()

    def _set_status(self, ticket_id: int, status: Status) -> None:
        entry = self._rows.get(ticket_id)
        if entry is None:
            self._rows[ticket_id] = RowEntry(
                ticket_id=ticket_id,
                status=status,
                report=None,
            )
        else:
            entry.status = status
        self._refresh_list()

    def _add_or_update_report(self, report: TriageReport) -> None:
        self._rows[report.ticket_id] = RowEntry(
            ticket_id=report.ticket_id,
            status="triaged",
            report=report,
        )
        self._refresh_list()

    def _set_failure(self, ticket_id: int, error: str) -> None:
        entry = self._rows.get(ticket_id)
        if entry is None:
            self._rows[ticket_id] = RowEntry(
                ticket_id=ticket_id,
                status="failed",
                report=None,
                failure_reason=error,
            )
        else:
            entry.status = "failed"
            entry.failure_reason = error
        self._refresh_list()

    def _selected_ticket_id(self) -> int | None:
        list_widget = self.query_one("#list", TicketListWidget)
        if list_widget.row_count and list_widget.is_valid_coordinate(
            list_widget.cursor_coordinate,
        ):
            row_key = list_widget.coordinate_to_cell_key(
                list_widget.cursor_coordinate,
            ).row_key.value
        else:
            row_key = None

        if row_key is not None:
            try:
                return int(row_key)
            except ValueError:
                return None

        current_report = self.query_one("#detail", ReportPaneWidget).current_report
        if current_report is not None:
            return current_report.ticket_id
        return None

    def _currently_selected(self) -> RowEntry | None:
        ticket_id = self._selected_ticket_id()
        return self._rows.get(ticket_id) if ticket_id is not None else None

    def action_cursor_up(self) -> None:
        self.query_one("#list", TicketListWidget).action_cursor_up()

    def action_cursor_down(self) -> None:
        self.query_one("#list", TicketListWidget).action_cursor_down()

    def action_focus_detail(self) -> None:
        self.query_one("#detail", ReportPaneWidget).focus()

    def action_focus_list(self) -> None:
        self.query_one("#list", TicketListWidget).focus()

    async def action_refresh(self) -> None:
        if self.poll_on_mount:
            self.run_worker(self._poll_tick(), exclusive=False)
            self.notify("Refreshing...", timeout=1)
            return

        hydrated_count = self._hydrate_recent_reports()
        self._last_poll = datetime.now(UTC)
        self._refresh_list()
        self.notify(f"Refreshed {hydrated_count} recent reports", timeout=1)

    def action_copy_note(self) -> None:
        entry = self._currently_selected()
        if entry is None or entry.report is None:
            self.notify("No triaged ticket selected", severity="warning", timeout=2)
            return

        if clipboard.copy_to_clipboard(entry.report.suggested_note):
            self.notify("Copied suggested note to clipboard", timeout=2)
            return

        self.notify(
            "No clipboard tool found (install wl-copy or xclip)",
            severity="warning",
            timeout=4,
        )

    def action_open_zendesk(self) -> None:
        entry = self._currently_selected()
        if entry is None:
            self.notify("No ticket selected", severity="warning", timeout=2)
            return

        subdomain = os.getenv("ZENDESK_SUBDOMAIN", "")
        if not subdomain:
            self.notify("ZENDESK_SUBDOMAIN not set", severity="warning", timeout=4)
            return

        url = f"https://{subdomain}.zendesk.com/agent/tickets/{entry.ticket_id}"
        self.notify(f"Open: {url}", timeout=10)
        webbrowser.open(url)

    def on_data_table_row_highlighted(self, event: DataTable.RowHighlighted) -> None:
        """When the list cursor moves, update the detail pane."""
        row_key = event.row_key.value
        if row_key is None:
            return
        try:
            ticket_id = int(row_key)
        except ValueError:
            return
        entry = self._rows.get(ticket_id)
        detail = self.query_one("#detail", ReportPaneWidget)
        detail.show(entry.report if entry is not None else None)
