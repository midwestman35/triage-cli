"""Textual widgets for the investigation-progress TUI."""
from __future__ import annotations

from textual.app import ComposeResult
from textual.widgets import Label, RichLog, Static

PHASES = [
    "fetch_ticket",
    "customer_history",
    "memory_lookup",
    "evidence_intake",
    "build_timeline",
    "enrichment",
    "llm_call",
    "save",
]

PHASE_LABELS = {
    "fetch_ticket": "Fetch ticket",
    "customer_history": "Customer history",
    "memory_lookup": "Memory lookup",
    "evidence_intake": "Evidence intake",
    "build_timeline": "Build timeline",
    "enrichment": "Enrichment",
    "llm_call": "LLM call",
    "save": "Save",
}

GLYPH_PENDING = "•"
GLYPH_ACTIVE  = "→"
GLYPH_DONE    = "✓"
GLYPH_FAILED  = "✗"


class WorkflowRail(Static):
    """Left pane: per-phase status list."""

    def __init__(self) -> None:
        super().__init__()
        self._states: dict[str, str] = {p: "pending" for p in PHASES}

    def compose(self) -> ComposeResult:
        for phase in PHASES:
            yield Label(self._render_phase(phase), id=f"phase-{phase}")

    def _render_phase(self, phase: str) -> str:
        state = self._states[phase]
        glyph = {
            "pending": GLYPH_PENDING,
            "active":  GLYPH_ACTIVE,
            "done":    GLYPH_DONE,
            "failed":  GLYPH_FAILED,
        }[state]
        return f"{glyph} {PHASE_LABELS.get(phase, phase)}"

    def set_phase(self, phase: str, state: str) -> None:
        self._states[phase] = state
        label = self.query_one(f"#phase-{phase}", Label)
        label.update(self._render_phase(phase))


class ActiveStepPane(Static):
    """Top-right pane: detail for the currently running step."""

    def update_step(self, phase: str, detail: str) -> None:
        self.update(f"[bold]{PHASE_LABELS.get(phase, phase)}[/bold]\n\n{detail}")


class TimelinePane(RichLog):
    """Bottom pane: append-only log of phase transitions."""

    def append_event(self, text: str) -> None:
        self.write(text)
