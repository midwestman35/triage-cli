"""Inbox Textual app: vertical-split list/detail with disk hydration on mount."""
from __future__ import annotations

import logging
from pathlib import Path

from textual.app import App, ComposeResult
from textual.containers import Horizontal
from textual.widgets import DataTable, Footer, Header

from triage_cli.inbox import hydrate
from triage_cli.inbox.widgets import ReportPaneWidget, RowEntry, TicketListWidget
from triage_cli.render import DEFAULT_OUTPUT_DIR
from triage_cli.watcher import WatcherOptions

logger = logging.getLogger(__name__)

class InboxApp(App):
    """Vertical-split inbox: TicketListWidget left, ReportPaneWidget right."""

    BINDINGS = [("q", "quit", "quit")]

    CSS = """
    Horizontal { height: 1fr; }
    TicketListWidget { width: 45%; min-width: 50; }
    ReportPaneWidget { width: 55%; }
    """

    TITLE = "triage-cli inbox"

    def __init__(self, opts: WatcherOptions, *, notes_dir: Path | None = None):
        super().__init__()
        self.opts = opts
        self.notes_dir = notes_dir if notes_dir is not None else DEFAULT_OUTPUT_DIR
        self._rows: dict[int, RowEntry] = {}

    def compose(self) -> ComposeResult:
        yield Header()
        with Horizontal():
            yield TicketListWidget(id="list")
            yield ReportPaneWidget(id="detail")
        yield Footer()

    async def on_mount(self) -> None:
        for report in hydrate.recent_reports(self.notes_dir, hours=24):
            self._rows[report.ticket_id] = RowEntry(
                ticket_id=report.ticket_id,
                status="triaged",
                report=report,
            )
        self._refresh_list()
        self.query_one("#detail", ReportPaneWidget).show(None)
        self.sub_title = f"view {self.opts.view_id} - {len(self._rows)} hydrated"

    def _refresh_list(self) -> None:
        list_widget = self.query_one("#list", TicketListWidget)
        list_widget.refresh_rows(list(self._rows.values()))

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
