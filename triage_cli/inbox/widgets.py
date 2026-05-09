"""Inbox row state model + DataTable / Static widgets."""
from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Literal

from textual.app import ComposeResult
from textual.containers import Vertical
from textual.widget import Widget
from textual.widgets import DataTable, Label, ProgressBar, Static

from triage_cli.models import TriageReport
from triage_cli.render import rich_layout

Status = Literal["triaged", "triaging", "queued", "failed"]

_STATUS_PRIORITY: dict[Status, int] = {
    "triaging": 0,
    "triaged": 1,
    "queued": 2,
    "failed": 3,
}

_STATUS_ICONS: dict[Status, str] = {
    "triaged": "✓",
    "triaging": "→",
    "queued": "○",
    "failed": "✗",
}

_STATUS_LABELS: dict[Status, str] = {
    "triaged": "triaged",
    "triaging": "triaging…",
    "queued": "in queue",
    "failed": "failed",
}

_CONFIDENCE_STYLE: dict[str, str] = {
    "high":   "[bold green]high[/]",
    "medium": "[yellow]med[/]",
    "low":    "[red]low[/]",
}

_ROW_STYLE: dict[Status, str | None] = {
    "failed":   "on dark_red",
    "triaging": "on dark_goldenrod",
    "triaged":  None,
    "queued":   None,
}

_SELECTED_ICON = "◉"


def _relative_time(dt: datetime, *, now: datetime | None = None) -> str:
    _now = now or datetime.now(UTC)
    minutes = int((_now - dt).total_seconds() / 60)
    if minutes < 2:
        return "just now"
    if minutes < 60:
        return f"{minutes}m ago"
    hours = minutes // 60
    if hours < 24:
        return f"{hours}h ago"
    return f"{(_now - dt).days}d ago"


@dataclass
class RowEntry:
    """In-memory state for one ticket in the inbox list."""

    ticket_id: int
    status: Status
    report: TriageReport | None
    site_hint: str | None = None
    failure_reason: str | None = None
    phase_label: str | None = None
    phase_step: int = 0


def sort_rows(rows: list[RowEntry]) -> list[RowEntry]:
    """Sort rows by status priority, then newest report first within a group."""
    epoch = datetime.fromtimestamp(0, tz=UTC)

    def key(row: RowEntry) -> tuple[int, float]:
        generated_at = row.report.generated_at if row.report is not None else epoch
        return (_STATUS_PRIORITY[row.status], -generated_at.timestamp())

    return sorted(rows, key=key)


class TicketListWidget(DataTable):
    """Inbox left pane: status icon, ticket number, site, time, confidence, summary."""

    def __init__(self, **kwargs):
        super().__init__(**kwargs, cursor_type="row", zebra_stripes=True)
        self._columns_added = False

    def on_mount(self) -> None:
        self._ensure_columns()

    def _ensure_columns(self) -> None:
        if self._columns_added:
            return
        self.add_columns(" ", "Ticket", "Site", "When", "Conf", "Summary")
        self._columns_added = True

    def refresh_rows(
        self,
        rows: list[RowEntry],
        *,
        selected_ticket_id: int | None = None,
    ) -> None:
        self._ensure_columns()
        self.clear()
        sorted_rows = sort_rows(rows)
        # Resolve where the cursor will land: caller's selection if it exists in
        # the new row set, otherwise the first row (Textual's default after add).
        # The ◉ marker tracks the resolved cursor row, not the requested one,
        # so the icon and the highlight stay in sync.
        cursor_ticket_id: int | None = None
        if selected_ticket_id is not None and any(
            r.ticket_id == selected_ticket_id for r in sorted_rows
        ):
            cursor_ticket_id = selected_ticket_id
        elif sorted_rows:
            cursor_ticket_id = sorted_rows[0].ticket_id

        selected_row = 0
        for row in sorted_rows:
            row_index = self.row_count
            report = row.report
            is_selected = row.ticket_id == cursor_ticket_id
            status_icon = _STATUS_ICONS[row.status]
            icon = f"{_SELECTED_ICON} {status_icon}" if is_selected else f"  {status_icon}"
            site = report.site_name if report is not None else row.site_hint or "—"
            when = _relative_time(report.generated_at) if report is not None else "—"
            confidence = (
                _CONFIDENCE_STYLE.get(report.confidence, report.confidence)
                if report is not None
                else "—"
            )
            summary = (
                report.finding[:60]
                if report is not None
                else row.failure_reason or _STATUS_LABELS[row.status]
            )
            style = _ROW_STYLE[row.status]
            if style:
                icon, ticket_col, site, when, confidence, summary = (
                    f"[{style}]{v}[/]"
                    for v in (icon, f"#{row.ticket_id}", site, when, confidence, summary)
                )
            else:
                ticket_col = f"#{row.ticket_id}"
            self.add_row(
                icon,
                ticket_col,
                site,
                when,
                confidence,
                summary,
                key=str(row.ticket_id),
            )
            if is_selected:
                selected_row = row_index

        if self.row_count:
            self.move_cursor(row=selected_row)


class ReportPaneWidget(Widget):
    """Inbox right pane: report view or triage progress bar."""

    can_focus = True

    DEFAULT_CSS = """
    ReportPaneWidget { padding: 1 2; overflow-y: auto; }
    #progress-region { align: center middle; height: 1fr; }
    #phase-label { margin-bottom: 1; }
    """

    current_report: TriageReport | None = None

    def compose(self) -> ComposeResult:
        yield Static(
            "[dim]Select a ticket to view its report.[/]",
            id="report-body",
        )
        with Vertical(id="progress-region"):
            yield Label("", id="phase-label")
            yield ProgressBar(total=4, show_eta=False, id="phase-bar")

    def on_mount(self) -> None:
        self.query_one("#progress-region").display = False

    def show_report(self, report: TriageReport) -> None:
        self.current_report = report
        self.query_one("#progress-region").display = False
        body = self.query_one("#report-body", Static)
        body.display = True
        body.update(rich_layout(report))

    def show_progress(self, label: str, step: int, total: int = 4) -> None:
        self.current_report = None
        self.query_one("#report-body", Static).display = False
        self.query_one("#progress-region").display = True
        self.query_one("#phase-label", Label).update(label)
        self.query_one("#phase-bar", ProgressBar).update(progress=step)

    def show_placeholder(self, text: str | None = None) -> None:
        self.current_report = None
        self.query_one("#progress-region").display = False
        body = self.query_one("#report-body", Static)
        body.display = True
        body.update(text or "[dim]Select a ticket to view its report.[/]")
