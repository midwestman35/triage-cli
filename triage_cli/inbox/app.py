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
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widgets import Button, DataTable, Footer, Header, Input, Label

from triage_cli import extract, pipeline, render, watcher
from triage_cli.datadog import DatadogClient
from triage_cli.inbox import clipboard, hydrate
from triage_cli.inbox.widgets import ReportPaneWidget, RowEntry, Status, TicketListWidget
from triage_cli.models import SiteEntry, Ticket, TriageReport
from triage_cli.render import DEFAULT_OUTPUT_DIR
from triage_cli.watcher import State, WatcherOptions
from triage_cli.zendesk import ZendeskClient

logger = logging.getLogger(__name__)


class SiteInputModal(ModalScreen):
    """Modal that prompts the user for a site_name when auto-resolution fails."""

    def __init__(self, ticket_id: int, subject: str, org: str | None) -> None:
        super().__init__()
        self._ticket_id = ticket_id
        self._subject = subject
        self._org = org

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(f"[bold]#{self._ticket_id}[/] {self._subject[:70]}")
            if self._org:
                yield Label(f"Org: [dim]{self._org}[/]")
            yield Label("[yellow]Could not auto-resolve site.[/] Enter site_name to query:")
            yield Input(placeholder="e.g. us-ga-roswell", id="site-input")
            with Horizontal(id="buttons"):
                yield Button("Triage", variant="primary", id="ok")
                yield Button("Cancel", id="cancel")

    def on_mount(self) -> None:
        self.query_one("#site-input", Input).focus()

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "ok":
            self._submit()
        else:
            self.dismiss(None)

    def on_input_submitted(self, _event: Input.Submitted) -> None:
        self._submit()

    def _submit(self) -> None:
        value = self.query_one("#site-input", Input).value.strip()
        self.dismiss(value if value else None)


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

    CSS_PATH = "inbox.tcss"

    TITLE = "triage-cli inbox"

    def __init__(
        self,
        opts: WatcherOptions,
        *,
        notes_dir: Path | None = None,
        poll_on_mount: bool = True,
        density: str = "comfortable",
    ):
        super().__init__()
        self.opts = opts
        self.notes_dir = notes_dir if notes_dir is not None else DEFAULT_OUTPUT_DIR
        self.poll_on_mount = poll_on_mount
        self._density = density
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
            yield TicketListWidget(id="list", density=self._density)
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
        for report in hydrate.recent_reports(
            self.notes_dir, hours=24, verbose=self.opts.verbose,
        ):
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
        view_label = str(self.opts.view_id) if self.opts.view_id is not None else "my tickets"
        if ticket_count == 0:
            self.sub_title = f"{view_label} - no tickets - last poll: {last_poll}"
        else:
            ticket_word = "ticket" if ticket_count == 1 else "tickets"
            self.sub_title = (
                f"{view_label} - {ticket_count} {ticket_word} - last poll: {last_poll}"
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
        new_state = None
        try:
            new_state = await asyncio.to_thread(
                self._run_iteration_blocking, self._state,
            )
        except RuntimeError as e:
            msg = str(e)
            self.notify(msg, severity="error", timeout=30)
            # "View X not found" is a permanent config mistake — exit instead of looping.
            # Guard narrowly: generic 404s (ticket, user) should not kill the app.
            if "view" in msg.lower() and "not found" in msg.lower():
                self.exit(1)
        finally:
            self._polling = False
            self._last_poll = datetime.now(UTC)
            if new_state is not None:
                self._state = new_state
            if self.is_running:
                self._refresh_list()

    def _run_iteration_blocking(self, state: State) -> State:
        """Run one watcher iteration in a worker thread; return the new state.

        Touches no ``self.*`` attributes for state mutation — the caller in
        ``_poll_tick`` reassigns ``self._state`` on the event-loop thread.

        Raises RuntimeError when the iteration aborts before listing the view
        (e.g. Zendesk auth failure, network error). ``_poll_tick`` converts
        that into a visible TUI notification.
        """
        sites = extract.load_site_map(Path("data/cnc-map.json"))

        stderr_buffer = io.StringIO()
        view_listed = False

        def _guarded_on_view_listed(ids: list[int]) -> None:
            nonlocal view_listed
            view_listed = True
            self._on_view_listed(ids)

        with contextlib.redirect_stderr(stderr_buffer), ZendeskClient.from_env() as zd:
            if self.opts.no_logs:
                new_state = watcher.run_iteration(
                    zd,
                    sites,
                    state,
                    self.opts,
                    self._backfill_cutoff,
                    dd_client=None,
                    on_view_listed=_guarded_on_view_listed,
                    on_progress=self._on_progress,
                    on_complete=self._on_complete,
                    on_failure=self._on_failure,
                )
            else:
                with DatadogClient.from_env() as dd:
                    new_state = watcher.run_iteration(
                        zd,
                        sites,
                        state,
                        self.opts,
                        self._backfill_cutoff,
                        dd_client=dd,
                        on_view_listed=_guarded_on_view_listed,
                        on_progress=self._on_progress,
                        on_complete=self._on_complete,
                        on_failure=self._on_failure,
                    )

        log_output = stderr_buffer.getvalue()
        for line in log_output.splitlines():
            logger.warning(line)

        if not view_listed:
            # run_iteration aborted before it could list the view — surface the
            # reason so _poll_tick can show it as a TUI notification.
            last_line = log_output.strip().rsplit("\n", 1)[-1] if log_output.strip() else ""
            raise RuntimeError(last_line or "Poll aborted — check inbox log for details")

        watcher.save_state(self.opts.state_file, watcher.prune_state(new_state))
        return new_state

    def _on_view_listed(self, view_ids: list[int]) -> None:
        if self.is_running:
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
                    status="queued",
                    report=None,
                )

        for ticket_id in list(self._rows):
            if self._rows[ticket_id].status == "queued" and ticket_id not in self._view_ids:
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

    async def action_focus_detail(self) -> None:
        entry = self._currently_selected()
        if entry is not None and entry.status == "queued":
            self.run_worker(self._triage_ticket_async(entry.ticket_id), exclusive=False)
            self.notify("Starting triage…", timeout=2)
            return
        self.query_one("#detail", ReportPaneWidget).focus()

    def action_focus_list(self) -> None:
        self.query_one("#list", TicketListWidget).focus()

    async def _triage_ticket_async(self, ticket_id: int) -> None:
        try:
            await asyncio.to_thread(self._triage_ticket_blocking, ticket_id)
        except Exception as e:
            self._on_failure(ticket_id, str(e))

    def _triage_ticket_blocking(self, ticket_id: int) -> None:
        """Fetch ticket, resolve site, run pipeline (worker thread).

        On site no_match, fires the SiteInputModal via call_from_thread and returns.
        The modal's dismiss handler restarts triage with the user-provided site override.
        """
        self.call_from_thread(self._set_status, ticket_id, "triaging")

        try:
            sites = extract.load_site_map(Path("data/cnc-map.json"))
        except (FileNotFoundError, ValueError) as e:
            self._on_failure(ticket_id, f"Site map error: {e}")
            return

        with ZendeskClient.from_env() as zd:
            try:
                ticket = zd.get_ticket(ticket_id)
            except RuntimeError as e:
                self._on_failure(ticket_id, str(e))
                return

        site_entry, _ = pipeline.resolve_site(ticket, sites, verbose=self.opts.verbose)
        if site_entry is None:
            # LLM also failed — hand off to the event loop to show the manual modal.
            self.call_from_thread(self._prompt_site_for_ticket, ticket, sites)
            return

        self._run_pipeline_blocking(ticket_id, ticket, site_entry)

    def _run_pipeline_blocking(
        self, ticket_id: int, ticket: Ticket, site_entry: SiteEntry
    ) -> None:
        """Run the triage pipeline for a resolved ticket+site (worker thread)."""
        try:
            if self.opts.no_logs:
                report = pipeline.triage_one(
                    ticket, site_entry, dd_client=None,
                    window_minutes=self.opts.window_minutes,
                    levels=self.opts.levels, at=None,
                    verbose=self.opts.verbose, show_spinner=False,
                )
            else:
                with DatadogClient.from_env() as dd:
                    report = pipeline.triage_one(
                        ticket, site_entry, dd_client=dd,
                        window_minutes=self.opts.window_minutes,
                        levels=self.opts.levels, at=None,
                        verbose=self.opts.verbose, show_spinner=False,
                    )
            render.save_note(report, ticket_id)
            self._on_complete(report)
        except (RuntimeError, ValueError) as e:
            self._on_failure(ticket_id, str(e))

    def _prompt_site_for_ticket(self, ticket: Ticket, sites: list[SiteEntry]) -> None:
        """Show the site-input modal (event loop). Called via call_from_thread."""
        def on_dismiss(site_name: str | None) -> None:
            if not site_name:
                self._set_failure(ticket.id, "Triage cancelled — no site provided")
                return
            site_entry, _ = extract.lookup_site(ticket, sites, site_override=site_name)
            self.run_worker(
                asyncio.to_thread(self._run_pipeline_blocking, ticket.id, ticket, site_entry),
                exclusive=False,
            )

        self.push_screen(
            SiteInputModal(ticket.id, ticket.subject, ticket.requester_org),
            on_dismiss,
        )

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
        if entry is None:
            detail.show(None)
        elif entry.status == "queued":
            detail.show(None, placeholder="[dim]○ In queue — press [bold]Enter[/] to triage now[/]")
        elif entry.status == "triaging":
            detail.show(None, placeholder="[dim]→ Triaging in progress…[/]")
        elif entry.status == "failed":
            reason = entry.failure_reason or "Unknown error"
            detail.show(None, placeholder=f"[red]✗ Triage failed:[/red]\n\n{reason}")
        else:
            detail.show(entry.report)
