"""Three-pane investigation-progress TUI."""
from __future__ import annotations

import asyncio

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.widgets import Footer, Header

from triage_cli.tui.reporter import (
    InvestigationDone,
    PhaseDone,
    PhaseFailed,
    PhaseStarted,
    ReporterEvent,
)
from triage_cli.tui.widgets import ActiveStepPane, TimelinePane, WorkflowRail


class InvestigationApp(App[None]):
    """Three-pane progress view for a single investigation."""

    BINDINGS = [
        Binding("q", "quit", "Quit"),
        Binding("tab", "focus_next", "Next pane"),
        Binding("shift+tab", "focus_previous", "Prev pane"),
    ]

    CSS = """
    Screen { layout: grid; grid-size: 2 2; }
    WorkflowRail { width: 30; border: solid $primary; padding: 1; }
    ActiveStepPane { border: solid $primary; padding: 1; }
    TimelinePane { column-span: 2; border: solid $primary; height: 12; }
    """

    def __init__(
        self,
        ticket_id: int,
        subject: str,
        queue: asyncio.Queue[ReporterEvent],
        pipeline_task: asyncio.Task,
    ) -> None:
        super().__init__()
        self._ticket_id = ticket_id
        self._subject = subject
        self._queue = queue
        self._pipeline_task = pipeline_task
        self._done = False

    def compose(self) -> ComposeResult:
        yield Header()
        yield WorkflowRail()
        yield ActiveStepPane()
        yield TimelinePane()
        yield Footer()

    def on_mount(self) -> None:
        self.title = f"triage-cli · ZD-{self._ticket_id} · {self._subject}"
        self.set_interval(0.1, self._poll_queue)

    async def _poll_queue(self) -> None:
        while not self._queue.empty():
            event = self._queue.get_nowait()
            self._handle_event(event)

    def _handle_event(self, event: ReporterEvent) -> None:
        rail = self.query_one(WorkflowRail)
        active = self.query_one(ActiveStepPane)
        timeline = self.query_one(TimelinePane)

        if isinstance(event, PhaseStarted):
            rail.set_phase(event.phase, "active")
            active.update_step(event.phase, event.detail or "running…")
            timeline.append_event(f"→ {event.phase}: {event.detail}")
        elif isinstance(event, PhaseDone):
            rail.set_phase(event.phase, "done")
            timeline.append_event(f"✓ {event.phase}: {event.detail}")
        elif isinstance(event, PhaseFailed):
            rail.set_phase(event.phase, "failed")
            timeline.append_event(f"✗ {event.phase}: {event.err}")
        elif isinstance(event, InvestigationDone):
            self._done = True
            self.sub_title = "complete — press q to exit"

    def action_quit(self) -> None:
        if not self._done and not self._pipeline_task.done():
            self._pipeline_task.cancel()
        self.exit()
